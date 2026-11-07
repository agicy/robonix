# robonix-llm-metrics

Rust writer of the per-request model usage record described in
[`robonix_api.llm`](../../pylib/robonix-api/robonix_api/llm/README.md). Pilot
uses it for every chat-completions request.

```rust
use robonix_llm_metrics::{Call, Status};

let mut call = Call::start("pilot", "plan", &base_url, &model, true, None);
// call.retry() for each resend; call.observe(&chunk) for each JSON body/chunk.
call.finish(Status::Ok, Some(200));
drop(call); // the record is written here
```

A call dropped without `finish` is recorded as `cancelled`. Records go to
`$ROBONIX_LLM_METRICS_DIR/<consumer>-<pid>.jsonl`; nothing is written when the
variable is unset, and a write failure is logged once through Scribe.

```bash
cargo test -p robonix-llm-metrics
```
