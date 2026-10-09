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

## Voice

Write the way Ruby's own docs talk to people: plain, friendly and short.

- Talk to the reader as "you".
- Show an example early. A good example often says more than a paragraph.
- Use the plain word. Do not invent a name for an idea the reader has
  never seen.
- One idea per sentence. Keep sentences short.
- Be warm, but do not sell. Say what iets does, and say plainly where it
  differs from Nix.
- Every fact must be true today. If something is not built yet, say so, or
  leave it out.
