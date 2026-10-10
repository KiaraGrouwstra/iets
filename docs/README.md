# How these docs are written

The docs follow [Diátaxis](https://diataxis.fr/). Each page has one job, and
the folder it lives in says which job that is. When a page starts doing a
second job, that part belongs on another page.

| folder | the reader wants to | the page gives them |
|---|---|---|
| `tutorial/` | learn, by doing | one guided path that always works |
| `how-to/` | get a task done | the steps for that task, and nothing else |
| `reference/` | look something up | complete, accurate facts, in tables where it fits |
| `explanation/` | understand why | the reasons, the tradeoffs, the history |

## What goes where

- **Tutorial.** You lead, and the reader follows. Every step works as
  written. Explain only what the reader needs for the next step, and link to
  an explanation for the rest.
- **How-to.** Start from the goal in the title: "Build remotely", "Fix a
  hash". Assume the reader knows the basics. Leave out choices that do not
  change the result.
- **Reference.** Describe what the reader can call, set or read, and what it
  does. Keep the same shape for every entry, so people and programs can scan
  it. Do not teach, and do not tell the story of the code.
- **Explanation.** Talk about why iets works the way it does. This is the
  place for design, tradeoffs, internals and dated measurements.

Code tours, file paths and struct names are for contributors. They go in
`ARCHITECTURE.md` or in the module's own doc comment, not on these pages.

## Voice and style

Write the way you'd explain it to a colleague at their desk: plain,
friendly and short. The mechanics follow the Google developer documentation
style guide, applied with judgment.

- Open a page with the reader's situation, not with a description of the
  page. "You have a derivation and a machine that should build it" beats
  "This page describes remote builds".
- Make a heading say what you can do or learn. On task pages it's a bare
  verb. On explanation pages it's a question or a plain claim, never a
  lone noun like "Scope" or "Why". Reference pages name the thing itself,
  in code font when it is a name.
- Contractions are fine, and so is an opinion, when it's true.
- Talk to the reader as "you".
- Show an example early. A good example often says more than a paragraph.
- Use the plain word. Do not invent a name for an idea the reader has
  never seen.
- One idea per sentence. Keep sentences short.
- Do not announce counts. Write "A getter on an unforced value returns
  `None`", not "Two rules keep a walk safe". List the items and let the
  reader see how many there are. Count only when the number is the fact:
  exit code 100, a limit of 500 paths.
- Say what is so, flatly. Name the mechanism, give the reason in one
  clause, and stop. No "it is worth noting", no "importantly", no tour
  of what the page will do.
- Say what iets does, and say plainly where it differs from Nix.
- Every fact must be true. If something is not built yet, say so, or
  leave it out. Avoid time words ("now", "today", "used to", "no longer")
  and put the history on the history page.
- Use sentence case in headings, without numbers. A task title is a bare
  verb ("Build on a remote builder"), not "How to …".
- Make link text the title of the page you link to, never a file path. The
  `reference/` pages are embedded in the binary (`iets help`), so they name
  other pages with a plain `docs/…` path in code font.
- Use American spelling, except Nix's own terms ("realise").
- Do not say "above" or "below" for a place on the page. Name the section.
- Introduce a list or a procedure with a full sentence.
