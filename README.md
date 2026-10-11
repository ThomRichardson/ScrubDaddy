<p align="center"><img src="assets/sponge.png" width="96" alt=""></p>

# Scrub Daddy

Drop PDF or Word documents (one, many, or a whole folder) on the little folder and Scrub Daddy scrubs every person's name
(except doctors and other credentialed professionals) and every claim/case number out of it, so the document is safe to paste into
a public AI tool. When it's done, drag the folder (now with a green check) anywhere to save the
scrubbed copy. Nothing is saved until you drop it somewhere.

- **PDFs, scanned or typed.** Every page is OCR'd twice, at two resolutions, with the OCR
  engine built into Windows (faxed and small print reads very differently at each), and the
  PDF's own text layer is read too. A name found in any pass is boxed. Names are painted out as plain black rectangles
  directly into the page pixels: each page is rebuilt as a single flattened image, so there is
  no overlay to remove and nothing underneath to copy out. The output keeps an invisible text
  layer of the remaining (non-redacted) words, so AI tools can still read it.
- **Word (.docx).** Names are replaced by solid black bars (`█████`) in the text itself, so
  nothing remains to unhide or copy. This covers the body and headers, footers,
  tables, text boxes, footnotes, comments and tracked changes. Author/editor metadata is
  wiped, `mailto:` links are removed, embedded pictures are OCR'd and boxed, and Word's hidden
  `[trash]` folder of old content is dropped.
- **Knows first, middle and last names,** and connects them: "Ms. Gonzalez", "Rebeca
  Gonzalez" and "GONZALEZ, REBECA" are recognized as the same person everywhere.
- **Professionals stay visible.** Anyone with credentials after their name ("Jill Henry, MA,
  CDMS", "Katie Hadley, ARNP", "Anna Stewart BA, VRCI"), anyone introduced by a professional
  role ("vocational counselor ...", "..., Vocational Testing Evaluator", "Analyst:",
  "Submitted By:", "represented by ..."), and doctors ("Dr.", "AP", physician labels) keep
  their names everywhere they appear in that document. Workers never have credentials, so a
  person listed as the worker/patient/claimant is always scrubbed, and a "Dr." who is also
  called Mr./Ms. somewhere is scrubbed too.
- **Claim/case numbers.** Catches two letters + digits (`BD78735`, `BL-77802`) and anything
  after a "Claim #:" / "Case No." label.
- The output is named `<name>-scrubbed.pdf` / `-scrubbed.docx` (names in the file name are
  scrubbed too). It waits in a private temp folder until you drag it out, and that folder is
  deleted when the app closes.
- Fast and offline: native Rust, multi-threaded OCR, nothing ever leaves your PC.

## How it finds names

1. Capitalized word runs are scored against ~22k first names (SSA) and ~218k surnames
   (US Census 2010 + international lists). Everyday-word meanings are discounted using a
   10k common-word list and how the word is used elsewhere in the same document.
2. Context raises confidence: titles (Mr., Dr.), credentials (`, MS, CDMS`), field labels
   (`Worker Name:`, `Client:`, `Re:`), greetings (`Dear`), and `Last, First` ordering.
3. Each confirmed name teaches the scrubber its parts, which are then removed everywhere
   else in the document: any capitalization, possessives, hyphenated forms, OCR typos
   (`Trijillo`), and email addresses built from them.

It errs on the side of over-scrubbing (e.g. a company called "Terry Martin Construction"
loses "Terry Martin"). **Always skim the result before uploading.** Detection is heuristic,
and handwriting and very poor scans can slip through.

## Using it

Download `ScrubDaddy.exe` from [Releases](../../releases) and run it. No install needed.

- Drop files on the window (or on the exe icon): any number of PDFs and .docx files, or
  whole folders (subfolders included; other file types are skipped). They are scrubbed one
  after another with one progress bar, and a file that fails doesn't stop the rest.
- Drag the green-check folder (it shows how many files it holds) to your desktop, a folder
  or an upload box. All the scrubbed files go together and are saved where you let go.
  Closing the app before then asks before discarding them.
- Double-click the folder to preview the file (or open them all in Explorer). Right-click to
  start over.
- Command line: `ScrubDaddy.exe --cli file1.pdf file2.docx` (saves next to each original).

Requires Windows 10 or 11 with an OCR language installed (English is there by default; see
Settings > Time & language > Language).

Only one window runs at a time: launching it again (or dropping a file on the exe) brings
the open window forward and hands it the file.

**Slow first launch?** The exe isn't code-signed, so antivirus tools (Avast's CyberCapture,
SmartScreen) may hold a new copy for 10–20 seconds the first time it runs while they check it.
Later launches open instantly. To skip the wait, add an exception for `ScrubDaddy.exe` (and
`%LOCALAPPDATA%\ScrubDaddy`, where the PDF engine is unpacked) in your antivirus.

## Building

Needs [Rust](https://rustup.rs) and Visual Studio Build Tools ("Desktop development with C++").

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1
```

This downloads the pinned PDF engine (PDFium) if needed, runs the tests, and writes
`dist\ScrubDaddy.exe`. The PDF engine is embedded in the exe, so it's a single file.

Developer aid: set `SCRUBDADDY_DEBUG=C:\path\dump.txt` to write the recognized text with each
redaction marked. That file contains the original names, so delete it afterwards.

## Releasing

Versions follow `MAJOR.MINOR.PATCH-beta.N`. Bump `version` in `Cargo.toml`, `resources.rc`
and `app.manifest`, build, tag `vX.Y.Z-beta.N`, and attach `dist\ScrubDaddy.exe` to a GitHub
pre-release.

## Credits

- [PDFium](https://pdfium.googlesource.com/pdfium/) via
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) and
  [pdfium-render](https://github.com/ajrcarey/pdfium-render). See [THIRD_PARTY.md](THIRD_PARTY.md).
- Name data: US Social Security Administration baby names, US Census Bureau 2010 surnames
  (public domain), [NameDatabases](https://github.com/smashew/NameDatabases) (Unlicense),
  [google-10000-english](https://github.com/first20hours/google-10000-english).
- The icon is a homemade smiley sponge in honor of the Scrub Daddy® sponge. This project
  isn't affiliated with Scrub Daddy, Inc.
