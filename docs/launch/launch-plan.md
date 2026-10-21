# Quarry launch plan

Publish the launch post only after the six-platform release is public and its
downloads are verified. Reach is uncertain; use a useful demo and real feedback as
the basis for the launch.

## Record a 45–60 second product demo

Use a disposable SQLite database created from examples/demo.sql. Show the real app
with readable text and captions; zoom into the active pane instead of shrinking the
entire desktop.

| Time | Show |
| --- | --- |
| 0–5 s | The question: “Which customers haven't ordered in the last 60 days?” |
| 5–18 s | The assistant inspects the schema and runs a read query. |
| 18–30 s | Open its SQL in the editor and show the result rows. |
| 30–45 s | Ask it to mark those customers inactive. Show the proposed SQL in the approval dialog and deny the change. |
| 45–60 s | Show the query result is unchanged, then the GitHub download page and source. |

Choose an AI backend you have configured. Model usage can incur provider charges.
Keep credentials, connection strings and unrelated desktop windows out of the recording.
The example data is fictional; it is suitable for a public demonstration.

## Launch sequence

1. Before posting, ask five relevant developers to try a download on their own OS.
   Ask whether it launched, connected and completed one query. Fix download or
   onboarding failures first. This is usability feedback, not a request to coordinate
   likes or comments.
2. Post docs/launch/linkedin-post.txt from your own profile with the short demo.
   Lead with the visible database workflow. Link directly to GitHub and ask the
   specific question at the end. Be available to answer actual questions afterward.
3. Share a short technical write-up in a Rust or database community where project
   launches are welcome. Explain the architecture, current limits and a concrete
   contribution opportunity. Follow each community's rules.
4. After 48–72 hours, post a follow-up showing one issue reported by a tester and
   the fix, or one query workflow people asked for. Credit contributors with permission.
5. At the end of the week, share what you learned: downloads, useful bug reports,
   working OS/database combinations and the next change. Use actual numbers.

LinkedIn's [creator guidance](https://members.linkedin.com/content/dam/me/members/en-us/pdf/bestpractices.pdf)
supports showing the work with images/video and encouraging a conversation.
Its [authentic engagement guidance](https://news.linkedin.com/2026/authentic-content-and-conversations)
explains that engagement pods and automated comments can reduce reach. The demo
length and sequence above are recommendations for this product, not algorithm guarantees.

## Improve conversion before chasing impressions

- Put the download link, OS/CPU choices and signing status near the top of the README.
- Show the app's approval dialog in the demo; it makes the write policy concrete.
- Keep the first trial small: the sample SQLite data needs no database server.
- Ask for an issue with OS, CPU, app version and steps when something fails.
- Track GitHub traffic, release asset downloads and actionable feedback. Ask testers
  whether they successfully connected and used the app; download counts alone do
  not establish active users.
- Complete Developer ID signing/notarization before a large macOS push if possible.
  The current ad hoc signing instructions add an installation step for users.

Do not claim production readiness, performance superiority, user counts, testimonials
or a successful audit unless you have evidence for them.
