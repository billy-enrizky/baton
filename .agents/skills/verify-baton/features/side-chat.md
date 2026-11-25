# Side chat

## Sub-features
- `/btw [prompt]` forks the current thread, marks the fork as a side chat of the source, and opens it.
- The source thread's messages, turns, and working tree are unchanged. The fork copies the source history so the side chat starts with the same context.
- A supplied prompt lands in the composer, not in a live turn, so the user reviews it before spending a provider turn.
- The banner shows `side chat of <source title>` while the side chat is open. Opening the source clears the marker.
- `/fork` remains the plain fork; only `/btw` writes the side-chat marker.

## How to get to it
Run one turn, then type `/btw why does this work?` or a bare `/btw`.

## Proof
- `side_chat_marks_the_source_and_leaves_it_unchanged`
- `btw_opens_a_marked_side_chat_and_prefills_the_prompt`
- The 53-case slash sweep runs `/btw` and requires the side-chat notice.

## Gotchas
- The side chat shares the source working tree, like `/fork`. It is not an isolated worktree; use `/worktree` for that.
- An open turn refuses the fork, so finish or interrupt the turn first.
- `/merge-back` is not implemented yet. The marker is the durable hook for it.
