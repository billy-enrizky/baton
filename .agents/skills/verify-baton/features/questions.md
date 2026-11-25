# Provider questions

## Sub-features
- Codex `item/tool/requestUserInput` becomes a question overlay instead of an unhandled server request.
- One request can carry multiple questions. The TUI walks them one at a time and sends one batched answers map.
- Each question lists its options. The selected option is sent as the answer.
- A question with `isOther` shows an `Other (type an answer)` row. Enter opens the custom input, Enter submits it, and Escape returns to the options.
- A question with `isSecret` masks the custom input with `*` while keeping the typed answer.
- A child-thread question names the first eight characters of the child thread id.
- The exec JSON stream includes the questions and never includes the responder.

## How to get to it (user POV)

Run a Codex turn that asks a clarifying question, for example from Plan mode or a prompt that asks Codex to request one. The question overlay appears above the composer. Use Up and Down to choose an option and Enter to answer. Choose Other when a custom answer is allowed, type the answer, and press Enter. Multiple questions are shown in order; the turn continues after the last answer.

## Approach

`TurnEvent::QuestionRequest` carries `QuestionPrompt` values and a responder for `Vec<QuestionAnswer>`. Codex `handle_user_input_request` parses `questions`, `options`, `isOther`, and `isSecret`, then `question_response` builds the native `{answers:{id:{answers:[...]}}}` result. The TUI stores a `QuestionState` per request, walks `index`, and `finish_question_answer` either advances to the next question or sends the batch. `question_key` handles the option cursor and the custom input; `draw_question` renders the header, wrapped question, options, custom row, and description.

## Gotchas
- Only Codex emits this event today. ACP questions and the other adapters still need their own mappings.
- The overlay shows six options at a time and keeps the selected row in the window. The Other row is always last.
- A child thread id is shortened to eight characters in the header.
- Multi-select is not implemented. Each question takes one answer.
- A secret answer is masked on screen but sent as typed, because the provider asked for the value.

## Proof

- `user_input_questions_keep_labels_custom_and_secret`
- `user_input_response_maps_native_ids_and_custom_answers`
- `question_events_do_not_leak_the_responder`
- `question_picker_answers_each_question_in_order`
- `question_picker_accepts_a_custom_answer`
- `child_question_overlay_names_the_child_and_lists_options`
- `secret_question_masks_the_custom_answer`
- `codex_adapter_routes_user_input_questions`
- `question_probe.py` runs a fake Codex app-server, answers an option and a custom note through the real TUI, and checks the answers map the server received.
