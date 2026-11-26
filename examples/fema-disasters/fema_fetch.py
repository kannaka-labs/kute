"""Fetch OpenFEMA Disaster Declarations Summaries v2 (public, keyless) into one CSV. Usage: fema_fetch.py OUT.csv"""
import csv, io, sys, time, urllib.parse, urllib.request

API = "https://www.fema.gov/api/open/v2/DisasterDeclarationsSummaries.csv"
SELECT = "disasterNumber,state,declarationType,declarationDate,incidentType,incidentBeginDate,fipsStateCode,fipsCountyCode"
rows, header, skip = [], None, 0
while True:
    q = urllib.parse.urlencode({"$select": SELECT, "$orderby": "declarationDate,disasterNumber,fipsStateCode,fipsCountyCode", "$top": 10000, "$skip": skip})
    req = urllib.request.Request(API + "?" + q, headers={"User-Agent": "python-urllib"})
    for attempt in range(4):
        try:
            text = urllib.request.urlopen(req, timeout=120).read().decode("utf-8-sig")
            break
        except Exception as e:
            print("retry", skip, e, file=sys.stderr)
            time.sleep(5)
    else:
        sys.exit("gave up at skip %d" % skip)
    page = list(csv.reader(io.StringIO(text)))
    if not page:
        break
    header, body = page[0], page[1:]
    rows += body
    print(skip, len(body), file=sys.stderr)
    if len(body) < 10000:
        break
    skip += 10000
with open(sys.argv[1], "w", newline="", encoding="utf-8") as f:
    w = csv.writer(f)
    w.writerow(header)
    w.writerows(rows)
print(len(rows), "rows", header)
