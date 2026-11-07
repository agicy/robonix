# robonix_api.llm

Shared helpers for Robonix components that call a model. `metrics` records
one JSON line per model request so usage can be counted, and later billed,
across every component.

## Usage

```python
from robonix_api.llm import metrics

with metrics.Call("scene", "relations", base_url, model) as call:
    response = await client.post(f"{base_url}/chat/completions", json=body)
    call.finish(response.status_code, response.json())
```

`finish(status, body)` reads the token usage from `body`. Leaving the block
without `finish` records `timeout`, `cancelled` or `transport_error` from
the exception, and the exception still propagates. A write failure is logged
once and never fails the model call.

## Record

Records go to `$ROBONIX_LLM_METRICS_DIR/<consumer>-<pid>.jsonl`. Nothing is
written when the variable is unset. `rbnx boot` sets it to
`<deploy>/rbnx-boot/llm-metrics/`, which boot and `rbnx clean -f` keep
unless `--sessions` is given.

| Field | Meaning |
|---|---|
| `v` | Layout version, currently 1 |
| `ts_ms`, `latency_ms` | Wall-clock end time and request duration |
| `boot_id`, `pid` | `RBNX_BOOT_ID` of the deployment and the writing process |
| `consumer`, `operation` | Component and what it used the model for |
| `host` | Endpoint host and port; never the path, query or credentials |
| `model`, `provider_model` | Model requested, and the one the response names |
| `catalog_id` | Model catalog entry, when the endpoint came from one |
| `stream` | Whether the reply was streamed |
| `status`, `http_status` | `ok`, `http_error`, `timeout`, `cancelled` or `transport_error` |
| `attempts` | Requests sent, counting retries (Rust writer only) |
| `input_tokens`, `output_tokens`, `cached_input_tokens`, `reasoning_tokens` | Reported counts; `null` when not reported |

Prompts, replies and keys are never recorded. Prices are not recorded either:
cost is token counts times the operator's price list, joined on `catalog_id`
or `host` + `model` when the records are aggregated.

The Rust crate `system/llm-metrics` writes the same record. Both parse usage
with the cases in [contract/usage_cases.json](contract/usage_cases.json).

| Consumer | Operations |
|---|---|
| `pilot` | `plan`, `summary` |
| `scene` | `relations`, `image_relations`, `caption`, `perception` |
| `memory` | `search`, `observe`, `answer` |
| `memsearch` | `compact` (tokens unknown: the memsearch library makes the request) |
| `vlm_verifier` | `judge` |

## Tests

```bash
PYTHONPATH=pylib/robonix-api python3 -m unittest pylib/robonix-api/tests/test_llm_metrics.py
cargo test -p robonix-llm-metrics
```
