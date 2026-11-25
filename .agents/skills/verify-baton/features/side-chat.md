# Side chat

## Sub-features
- `/btw [prompt]` forks the current thread, marks the fork as a side chat of the source, and opens it.
- The source thread's messages, turns, and working tree are unchanged. The fork copies the source history so the side chat starts with the same context.
- A supplied prompt lands in the composer, not in a live turn, so the user reviews it before spending a provider turn.
- The banner shows `side chat of <source title>` while the side chat is open. Opening the source clears the marker.
- `/fork` remains the plain fork; only `/btw` writes the side-chat marker.
- `/merge-back [prompt]` appends the side chat's messages created after the fork to the source as one redacted "Merge-back context" message, opens the source, and prefills the prompt. A source with open work refuses the merge.

## How to get to it
Run one turn, then type `/btw why does this work?` or a bare `/btw`.

## Proof
- `side_chat_marks_the_source_and_leaves_it_unchanged`
- `btw_opens_a_marked_side_chat_and_prefills_the_prompt`
- `merge_back_moves_the_delta_and_redacts_keys`
- `merge_back_requires_a_side_chat_and_a_delta`
- The 53-case slash sweep runs `/btw` and requires the side-chat notice.

## Gotchas
- The side chat shares the source working tree, like `/fork`. It is not an isolated worktree; use `/worktree` for that.
- An open turn refuses the fork, so finish or interrupt the turn first.
- Merge-back transfers the conversation delta, not the checkpoint. The side chat shares the source working tree, and the summary says so.
