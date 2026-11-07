# SPDX-License-Identifier: MulanPSL-2.0
from __future__ import annotations

import asyncio
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from robonix_api.llm import metrics

CASES = Path(metrics.__file__).parent / "contract" / "usage_cases.json"


class UsageFromResponseTest(unittest.TestCase):
    def test_shared_cases(self) -> None:
        for case in json.loads(CASES.read_text())["cases"]:
            with self.subTest(case["name"]):
                self.assertEqual(metrics.usage_from_response(case["body"]), case["expected"])


class Response:
    def __init__(self, status_code: int, body: object) -> None:
        self.status_code = status_code
        self.body = body

    def json(self) -> object:
        if isinstance(self.body, str):
            raise ValueError("not JSON")
        return self.body


class CallTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        patcher = mock.patch.dict(os.environ, {
            "ROBONIX_LLM_METRICS_DIR": self.directory.name, "RBNX_BOOT_ID": "boot-1",
        })
        patcher.start()
        self.addCleanup(patcher.stop)

    def records(self) -> list[dict]:
        path = Path(self.directory.name) / f"scene-{os.getpid()}.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()]

    def test_reply_is_recorded_without_credentials_or_path(self) -> None:
        body = {"usage": {"prompt_tokens": 10, "completion_tokens": 2}}
        with metrics.Call("scene", "relations", "https://user:k@api.example.com/v1?key=s", "m") as call:
            call.finish(Response(200, body))
        [record] = self.records()
        self.assertEqual(record["host"], "api.example.com")
        self.assertEqual((record["status"], record["input_tokens"], record["output_tokens"]), ("ok", 10, 2))
        self.assertEqual((record["consumer"], record["operation"], record["boot_id"]), ("scene", "relations", "boot-1"))
        self.assertNotIn("key", json.dumps(record))

    def test_failures_are_recorded_and_still_raise(self) -> None:
        class ReadTimeout(Exception):
            pass

        with metrics.Call("scene", "a", "http://127.0.0.1:8000/v1", "m") as call:
            call.finish(Response(429, {"error": {}}))
        with metrics.Call("scene", "d", "", "m") as call:
            call.finish(Response(200, "data: not json"))
        with self.assertRaises(ReadTimeout), metrics.Call("scene", "b", "", "m"):
            raise ReadTimeout()

        async def cancelled() -> None:
            with metrics.Call("scene", "c", "", "m"):
                raise asyncio.CancelledError()

        with self.assertRaises(asyncio.CancelledError):
            asyncio.run(cancelled())
        statuses = [(r["host"], r["status"], r["http_status"]) for r in self.records()]
        self.assertEqual(statuses, [
            ("127.0.0.1:8000", "http_error", 429), ("", "ok", 200),
            ("", "timeout", None), ("", "cancelled", None),
        ])

    def test_unset_or_unwritable_directory_does_not_fail_the_call(self) -> None:
        with mock.patch.dict(os.environ, {"ROBONIX_LLM_METRICS_DIR": ""}):
            with metrics.Call("scene", "a", "", "m") as call:
                call.finish()
        blocker = Path(self.directory.name) / "file"
        blocker.write_text("")
        with mock.patch.dict(os.environ, {"ROBONIX_LLM_METRICS_DIR": str(blocker / "sub")}):
            with metrics.Call("scene", "a", "", "m") as call:
                call.finish()
        self.assertEqual(os.listdir(self.directory.name), ["file"])


if __name__ == "__main__":
    unittest.main()
