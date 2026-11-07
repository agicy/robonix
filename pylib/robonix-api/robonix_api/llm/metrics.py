# SPDX-License-Identifier: MulanPSL-2.0
"""One JSON line per model request, for usage metrics and later billing.

Records go to ``$ROBONIX_LLM_METRICS_DIR/<consumer>-<pid>.jsonl``; nothing is
written when the variable is unset. Prompts, replies and keys are never
recorded, and the endpoint is reduced to its host. The Rust crate
``robonix-llm-metrics`` writes the same record; both parse usage with the
cases in ``contract/usage_cases.json``.
"""
from __future__ import annotations

import json
import logging
import os
import threading
import time
from pathlib import Path
from typing import Any, Optional
from urllib.parse import urlsplit

log = logging.getLogger(__name__)

SCHEMA_VERSION = 1
_lock = threading.Lock()
_warned = False


def usage_from_response(body: Any) -> dict[str, Any]:
    """Token counts from a Chat Completions body or stream chunk; absent ones are None."""
    usage = body.get("usage") if isinstance(body, dict) else None
    if not isinstance(usage, dict):
        usage = {}
    prompt = usage.get("prompt_tokens_details")
    completion = usage.get("completion_tokens_details")
    cached = prompt.get("cached_tokens") if isinstance(prompt, dict) else None
    if cached is None:
        # DeepSeek reports cache hits outside prompt_tokens_details.
        cached = usage.get("prompt_cache_hit_tokens")
    model = body.get("model") if isinstance(body, dict) else None
    return {
        "input_tokens": _count(usage.get("prompt_tokens")),
        "output_tokens": _count(usage.get("completion_tokens")),
        "cached_input_tokens": _count(cached),
        "reasoning_tokens": _count(
            completion.get("reasoning_tokens") if isinstance(completion, dict) else None
        ),
        "provider_model": model if isinstance(model, str) and model else None,
    }


def _count(value: Any) -> Optional[int]:
    return value if type(value) is int and value >= 0 else None


def _host(base_url: str) -> str:
    parts = urlsplit(base_url or "")
    host = parts.hostname or ""
    return f"{host}:{parts.port}" if host and parts.port else host


class Call:
    """One model request; use as a context manager around the HTTP exchange.

    ``finish`` stores the outcome. Leaving the block without it records the
    exception kind (``timeout``, ``cancelled`` or ``transport_error``), so a
    failed request is still counted: providers can bill those too.
    """

    def __init__(
        self,
        consumer: str,
        operation: str,
        base_url: str,
        model: str,
        *,
        stream: bool = False,
        catalog_id: Optional[str] = None,
    ) -> None:
        self.record: dict[str, Any] = {
            "v": SCHEMA_VERSION,
            "consumer": consumer,
            "operation": operation,
            "host": _host(base_url),
            "model": model,
            "catalog_id": catalog_id,
            "stream": stream,
        }
        self._started = 0.0
        self._done = False

    def __enter__(self) -> "Call":
        self._started = time.monotonic()
        return self

    def finish(self, response: Any = None) -> None:
        """Record an HTTP response (``status_code`` and ``json()``, as httpx has).

        Without a response the call is ``ok`` with unknown usage. A body that is
        not JSON leaves usage unknown; the request was still answered.
        """
        status = getattr(response, "status_code", None)
        try:
            body = response.json() if response is not None else None
        except ValueError:
            body = None
        self.record["status"] = "ok" if status is None or status < 400 else "http_error"
        self.record["http_status"] = status
        self.record.update(usage_from_response(body))
        self._done = True

    def __exit__(self, exc_type, _exc, _tb) -> bool:
        if not self._done:
            name = exc_type.__name__ if exc_type else ""
            if "Timeout" in name:
                status = "timeout"
            elif name == "CancelledError":
                status = "cancelled"
            else:
                status = "transport_error"
            self.record.update(status=status, http_status=None, **usage_from_response(None))
        self.record["latency_ms"] = round((time.monotonic() - self._started) * 1000)
        write(self.record)
        return False


def write(record: dict[str, Any]) -> None:
    """Append ``record`` with timestamp, boot and process ids; never raises."""
    global _warned  # noqa: PLW0603
    directory = os.environ.get("ROBONIX_LLM_METRICS_DIR", "").strip()
    if not directory:
        return
    line = dict(record, ts_ms=int(time.time() * 1000),
                boot_id=os.environ.get("RBNX_BOOT_ID") or None, pid=os.getpid())
    path = Path(directory) / f"{record['consumer']}-{os.getpid()}.jsonl"
    try:
        with _lock:
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("a", encoding="utf-8") as sink:
                sink.write(json.dumps(line, ensure_ascii=False, separators=(",", ":")) + "\n")
    except OSError as error:
        # A metrics failure must not fail the model call; say so once.
        if not _warned:
            _warned = True
            log.warning("llm metrics: cannot write %s: %s", path, error)
