"""Builds the embedded name dictionaries in data/ from public datasets.

Usage: python tools/build_data.py <download_dir>

Expected files in <download_dir>:
  census.zip                 US Census 2010 surnames (public domain)
                             https://www2.census.gov/topics/genealogy/2010surnames/names.zip
  hadley.csv                 SSA baby names, top 1000/year 1880-2008 (public domain data)
                             https://github.com/hadley/data-baby-names
  smashew_first_all.txt      International first names (Unlicense)
  smashew_sur_all.txt        International surnames (Unlicense)
                             https://github.com/smashew/NameDatabases
  common.txt                 10k most common English words
                             https://github.com/first20hours/google-10000-english
"""
import csv, io, math, os, sys, unicodedata, zipfile

src = sys.argv[1]
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data")


def norm(s):
    s = s.strip().lstrip("﻿")
    s = unicodedata.normalize("NFC", s)
    return s.lower()


def ok(s):
    return len(s) >= 2 and all(c.isalpha() or c in "'-" for c in s)


# ---- first names: strength 1..3 from cumulative SSA popularity ----
first = {}
pop = {}
with open(os.path.join(src, "hadley.csv"), newline="", encoding="utf-8") as f:
    for row in csv.DictReader(f):
        n = norm(row["name"])
        pop[n] = pop.get(n, 0.0) + float(row["percent"])
for n, p in pop.items():
    if not ok(n):
        continue
    # p is a sum of yearly shares across 129 years
    first[n] = 3 if p >= 0.05 else 2 if p >= 0.005 else 1
with open(os.path.join(src, "smashew_first_all.txt"), encoding="utf-8") as f:
    for line in f:
        n = norm(line)
        if ok(n):
            first.setdefault(n, 1)

# ---- surnames: strength 1..3 from census rank ----
sur = {}
with zipfile.ZipFile(os.path.join(src, "census.zip")) as z:
    with z.open("Names_2010Census.csv") as f:
        for row in csv.DictReader(io.TextIOWrapper(f, encoding="utf-8")):
            n = norm(row["name"])
            if not ok(n) or n == "all other names":
                continue
            rank = int(row["rank"])
            sur[n] = 3 if rank <= 2000 else 2 if rank <= 25000 else 1
with open(os.path.join(src, "smashew_sur_all.txt"), encoding="utf-8") as f:
    for line in f:
        n = norm(line)
        if ok(n):
            sur.setdefault(n, 1)

# ---- common words with rank ----
common = []
with open(os.path.join(src, "common.txt"), encoding="utf-8") as f:
    for line in f:
        w = norm(line)
        if w.isalpha():
            common.append(w)

os.makedirs(out, exist_ok=True)
with open(os.path.join(out, "first_names.tsv"), "w", encoding="utf-8", newline="\n") as f:
    for n in sorted(first):
        f.write(f"{n}\t{first[n]}\n")
with open(os.path.join(out, "surnames.tsv"), "w", encoding="utf-8", newline="\n") as f:
    for n in sorted(sur):
        f.write(f"{n}\t{sur[n]}\n")
with open(os.path.join(out, "common_words.txt"), "w", encoding="utf-8", newline="\n") as f:
    f.write("\n".join(common) + "\n")
print(f"first={len(first)} surnames={len(sur)} common={len(common)}")
