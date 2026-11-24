//! Algorithm core shared by byte BPE and symbol-sequence BPE.
//!
//! Works on sequences of `u32` ids over any base alphabet. Selection rule,
//! chosen to match rustbpe and tiktoken: merge the most frequent adjacent pair;
//! on a tie, the numerically smallest pair. The result depends only on the
//! multiset of input sequences, never on hash or thread order.

use rustc_hash::{FxHashMap, FxHashSet};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub type Pair = (u32, u32);

/// One learned merge and the number of times the pair occurred when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LearnedMerge {
    pub left: u32,
    pub right: u32,
    pub id: u32,
    pub count: u64,
}

/// Learn up to `max_merges` merges.
///
/// `words[i]` occurs `counts[i]` times. New ids start at `first_id`. A pair is
/// only merged while its count is at least `min_frequency` and `allow(left,
/// right)` returns true (used for the token length limit).
pub fn train_merges(
    mut words: Vec<Vec<u32>>,
    counts: &[u64],
    first_id: u32,
    max_merges: u32,
    min_frequency: u64,
    mut allow: impl FnMut(u32, u32) -> bool,
) -> Vec<LearnedMerge> {
    debug_assert_eq!(words.len(), counts.len());
    let mut pair_counts: FxHashMap<Pair, i64> = FxHashMap::default();
    let mut where_used: FxHashMap<Pair, FxHashSet<u32>> = FxHashMap::default();
    for (i, w) in words.iter().enumerate() {
        if counts[i] == 0 {
            continue;
        }
        for p in w.windows(2) {
            let pair = (p[0], p[1]);
            *pair_counts.entry(pair).or_insert(0) += counts[i] as i64;
            where_used.entry(pair).or_default().insert(i as u32);
        }
    }

    let mut heap: BinaryHeap<(i64, Reverse<Pair>)> =
        pair_counts.iter().filter(|(_, &c)| c > 0).map(|(&p, &c)| (c, Reverse(p))).collect();
    let mut banned: FxHashSet<Pair> = FxHashSet::default();
    let mut merges = Vec::new();
    let min_frequency = min_frequency.max(1) as i64;

    while (merges.len() as u32) < max_merges {
        let Some((count, Reverse(pair))) = heap.pop() else { break };
        let current = pair_counts.get(&pair).copied().unwrap_or(0);
        if current <= 0 || banned.contains(&pair) {
            continue;
        }
        if current != count {
            // Stale entry: the count changed after it was queued.
            heap.push((current, Reverse(pair)));
            continue;
        }
        if current < min_frequency {
            break;
        }
        if !allow(pair.0, pair.1) {
            banned.insert(pair);
            continue;
        }

        let new_id = first_id + merges.len() as u32;
        merges.push(LearnedMerge { left: pair.0, right: pair.1, id: new_id, count: current as u64 });

        let mut touched: FxHashSet<Pair> = FxHashSet::default();
        let users = where_used.remove(&pair).unwrap_or_default();
        for word_index in users {
            let i = word_index as usize;
            let word = &words[i];
            if !word.windows(2).any(|p| (p[0], p[1]) == pair) {
                continue;
            }
            let weight = counts[i] as i64;
            for p in word.windows(2) {
                let old = (p[0], p[1]);
                *pair_counts.entry(old).or_insert(0) -= weight;
                touched.insert(old);
            }
            let mut merged = Vec::with_capacity(word.len());
            let mut j = 0;
            while j < word.len() {
                if j + 1 < word.len() && word[j] == pair.0 && word[j + 1] == pair.1 {
                    merged.push(new_id);
                    j += 2;
                } else {
                    merged.push(word[j]);
                    j += 1;
                }
            }
            for p in merged.windows(2) {
                let new = (p[0], p[1]);
                *pair_counts.entry(new).or_insert(0) += weight;
                where_used.entry(new).or_default().insert(word_index);
                touched.insert(new);
            }
            words[i] = merged;
        }
        pair_counts.remove(&pair);
        for p in touched {
            if p == pair {
                continue;
            }
            match pair_counts.get(&p).copied() {
                Some(c) if c > 0 => heap.push((c, Reverse(p))),
                Some(_) => {
                    pair_counts.remove(&p);
                }
                None => {}
            }
        }
    }
    merges
}

/// Apply learned merges to one sequence.
///
/// Equivalent to repeatedly merging, left to right, every occurrence of the
/// applicable pair with the lowest merged id, which is what tiktoken and
/// rustbpe do, but in `O(n log n)` so that megabyte inputs without a
/// pre-tokenizer stay fast.
pub fn apply_merges(ids: &mut Vec<u32>, ranks: &FxHashMap<Pair, u32>) {
    let n = ids.len();
    if n < 2 || ranks.is_empty() {
        return;
    }
    if n <= 16 {
        apply_merges_small(ids, ranks);
        return;
    }
    const NONE: u32 = u32::MAX;
    let mut next: Vec<u32> = (1..=n as u32).collect();
    next[n - 1] = NONE;
    let mut prev: Vec<u32> = (0..n as u32).map(|i| i.wrapping_sub(1)).collect();
    prev[0] = NONE;
    let mut alive = vec![true; n];
    let initial: Vec<Reverse<(u32, u32)>> = (0..n - 1)
        .filter_map(|i| ranks.get(&(ids[i], ids[i + 1])).map(|&rank| Reverse((rank, i as u32))))
        .collect();
    if initial.is_empty() {
        return;
    }
    let mut heap = BinaryHeap::from(initial);
    while let Some(Reverse((rank, pos))) = heap.pop() {
        let i = pos as usize;
        if !alive[i] {
            continue;
        }
        let j = next[i];
        if j == NONE {
            continue;
        }
        let j = j as usize;
        if ranks.get(&(ids[i], ids[j])) != Some(&rank) {
            continue;
        }
        ids[i] = rank;
        alive[j] = false;
        let after = next[j];
        next[i] = after;
        if after != NONE {
            prev[after as usize] = pos;
            if let Some(&r) = ranks.get(&(rank, ids[after as usize])) {
                heap.push(Reverse((r, pos)));
            }
        }
        let before = prev[i];
        if before != NONE {
            if let Some(&r) = ranks.get(&(ids[before as usize], rank)) {
                heap.push(Reverse((r, before)));
            }
        }
    }
    let mut write = 0;
    for read in 0..n {
        if alive[read] {
            ids[write] = ids[read];
            write += 1;
        }
    }
    ids.truncate(write);
}

fn apply_merges_small(ids: &mut Vec<u32>, ranks: &FxHashMap<Pair, u32>) {
    while ids.len() >= 2 {
        let mut best: Option<(usize, u32)> = None;
        for i in 0..ids.len() - 1 {
            if let Some(&rank) = ranks.get(&(ids[i], ids[i + 1])) {
                if best.map_or(true, |(_, b)| rank < b) {
                    best = Some((i, rank));
                }
            }
        }
        let Some((_, rank)) = best else { break };
        // Merge every occurrence of the winning pair, left to right.
        let mut write = 0;
        let mut read = 0;
        while read < ids.len() {
            if read + 1 < ids.len() && ranks.get(&(ids[read], ids[read + 1])) == Some(&rank) {
                ids[write] = rank;
                read += 2;
            } else {
                ids[write] = ids[read];
                read += 1;
            }
            write += 1;
        }
        ids.truncate(write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranks_of(merges: &[LearnedMerge]) -> FxHashMap<Pair, u32> {
        merges.iter().map(|m| ((m.left, m.right), m.id)).collect()
    }

    /// Textbook quadratic reference used to check both implementations.
    fn reference(mut ids: Vec<u32>, ranks: &FxHashMap<Pair, u32>) -> Vec<u32> {
        loop {
            let best = ids.windows(2).filter_map(|p| ranks.get(&(p[0], p[1])).copied()).min();
            let Some(rank) = best else { return ids };
            let mut out = Vec::new();
            let mut i = 0;
            while i < ids.len() {
                if i + 1 < ids.len() && ranks.get(&(ids[i], ids[i + 1])) == Some(&rank) {
                    out.push(rank);
                    i += 2;
                } else {
                    out.push(ids[i]);
                    i += 1;
                }
            }
            ids = out;
        }
    }

    #[test]
    fn picks_most_frequent_then_smallest_pair() {
        // "ab" x3 and "cd" x3 tie; (97,98) is the smaller pair.
        let words = vec![vec![97, 98], vec![99, 100]];
        let merges = train_merges(words, &[3, 3], 256, 2, 1, |_, _| true);
        assert_eq!((merges[0].left, merges[0].right, merges[0].id, merges[0].count), (97, 98, 256, 3));
        assert_eq!((merges[1].left, merges[1].right, merges[1].id), (99, 100, 257));
    }

    #[test]
    fn overlapping_pairs_count_correctly() {
        // "aaaa": pair (a,a) occurs 3 times, merges to X X, then (X,X).
        let merges = train_merges(vec![vec![1, 1, 1, 1]], &[1], 10, 5, 1, |_, _| true);
        assert_eq!(merges.len(), 2);
        assert_eq!((merges[0].left, merges[0].right, merges[0].count), (1, 1, 3));
        assert_eq!((merges[1].left, merges[1].right, merges[1].count), (10, 10, 1));
    }

    #[test]
    fn respects_min_frequency_and_allow() {
        let words = vec![vec![1, 2, 3], vec![1, 2]];
        let merges = train_merges(words.clone(), &[5, 1], 10, 9, 2, |_, _| true);
        // (1,2) x6 then (10,3) x5; nothing else reaches 2.
        assert_eq!(merges.len(), 2);
        let none = train_merges(words, &[5, 1], 10, 9, 1, |_, _| false);
        assert!(none.is_empty());
    }

    #[test]
    fn fast_apply_matches_reference() {
        let mut state = 0x9e3779b97f4a7c15u64;
        let mut rand = move |m: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % m as u64) as u32
        };
        for round in 0..60 {
            let alphabet = 2 + rand(5);
            let words: Vec<Vec<u32>> = (0..20).map(|_| (0..2 + rand(40)).map(|_| rand(alphabet)).collect()).collect();
            let counts: Vec<u64> = (0..20).map(|_| 1 + rand(4) as u64).collect();
            let merges = train_merges(words, &counts, alphabet, 30, 1, |_, _| true);
            let ranks = ranks_of(&merges);
            for len in [0usize, 1, 2, 5, 16, 17, 64, 300] {
                let input: Vec<u32> = (0..len).map(|_| rand(alphabet)).collect();
                let expected = reference(input.clone(), &ranks);
                let mut fast = input.clone();
                apply_merges(&mut fast, &ranks);
                assert_eq!(fast, expected, "round {round} len {len}");
                let mut small = input;
                apply_merges_small(&mut small, &ranks);
                assert_eq!(small, expected, "small path, round {round} len {len}");
            }
        }
    }
}
