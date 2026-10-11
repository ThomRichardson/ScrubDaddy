<p align="center"><img src="assets/sponge.png" width="96" alt=""></p>

# Scrub Daddy

Drop a PDF or Word document on the little folder and Scrub Daddy scrubs every person's name
and every claim/case number out of it, so the document is safe to paste into a public AI tool.
When it's done, drag the folder (now with a green check) anywhere to save the scrubbed copy.

- **PDFs, scanned or typed.** Every page is OCR'd with the OCR engine built into Windows,
  and the PDF's own text layer is read too. Names are burned out as solid black boxes, and the
  page is rebuilt as an image, so nothing underneath can be copied back out. The output keeps
  an invisible text layer of the remaining words, so AI tools can still read it.
- **Word (.docx).** Names are replaced in the text itself, including headers, footers,
  tables, text boxes, footnotes, comments and tracked changes. Author/editor metadata is
  wiped, `mailto:` links are removed, embedded pictures are OCR'd and boxed, and Word's hidden
  `[trash]` folder of old content is dropped.
- **Knows first, middle and last names.** Names become consistent labels like
  `[P1 FIRST] [P1 LAST]`, so "Ms. Gonzalez" and "Rebeca Gonzalez" are both `P1`. The AI can
  still follow who's who without ever seeing a name.
- **Claim/case numbers.** Catches two letters + digits (`BD78735`, `BL-77802`) and anything
  after a "Claim #:" / "Case No." label.
- The output is saved next to the original as `<name>-scrubbed.pdf` / `-scrubbed.docx`.
  Names in the file name are scrubbed too.
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

- Drop a file on the window (or on the exe icon). The progress bar shows the scrub.
- Drag the green-check folder to your desktop, a folder or an upload box. It moves the file.
- Double-click the folder to show the file in Explorer. Right-click to start over.
- Command line: `ScrubDaddy.exe --cli file1.pdf file2.docx`

Requires Windows 10 or 11 with an OCR language installed (English is there by default; see
Settings > Time & language > Language).

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

Versions follow `MAJOR.MINOR.PATCH-beta.N`. Bump `version` in `Cargo.toml` and
`resources.rc`, build, tag `vX.Y.Z-beta.N`, and attach `dist\ScrubDaddy.exe` to a GitHub
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
