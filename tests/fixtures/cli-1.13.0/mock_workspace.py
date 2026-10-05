#!/usr/bin/env python3
"""
Mock Databricks workspace server for testing.

Uses Python standard library only. Binds to 127.0.0.1 on an ephemeral port.
Returns recorded responses deterministically and refuses unrecorded requests.
Never forwards to a real workspace.

Usage:
    python3 mock_workspace.py [--host HOST] [--port PORT]

Example:
    python3 mock_workspace.py --port 8000
"""

import argparse
import json
import http.server
import socket
import socketserver
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple


# Recorded responses: (method, path) -> (status_code, headers, body)
RESPONSES: Dict[Tuple[str, str], Tuple[int, Dict[str, str], str]] = {
    ("GET", "/api/2.1/pipelines/01a23b45c67d8901"): (
        200,
        {"Content-Type": "application/json"},
        json.dumps(
            {
                "object_type": "PIPELINE",
                "object_id": "01a23b45c67d8901",
                "name": "example-pipeline",
                "creator_user_id": 1234567890,
                "created_at": 1698000000,
                "updated_at": 1698100000,
                "pipeline_type": "TRIGGERED",
                "clusters": [
                    {
                        "label": "default",
                        "spark_conf": {
                            "spark.databricks.cluster.profile": "singleNode"
                        },
                        "node_type_id": "i3.xlarge",
                        "num_workers": 0,
                        "aws_attributes": {
                            "availability": "SPOT",
                            "zone_id": "us-west-2b",
                        },
                    }
                ],
                "storage": "/Workspace/Users/user@example.test/projects/example",
                "configuration": {
                    "q": {
                        "id": "abcd1234-ef56-7890-abcd-ef1234567890",
                        "storage": "/Workspace/Users/user@example.test/projects/example/dlt_config",
                        "notebooks": [
                            {
                                "path": "/Workspace/Users/user@example.test/projects/example/notebooks/query"
                            }
                        ],
                    }
                },
                "channel": "CURRENT",
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
    ),
    ("GET", "/api/2.1/jobs/123"): (
        200,
        {"Content-Type": "application/json"},
        json.dumps(
            {
                "job_id": 123,
                "creator_user_id": 1234567890,
                "run_as_user_id": 1234567890,
                "run_as_principal_user_name": "user@example.test",
                "created_time": 1698000000000,
                "settings": {
                    "name": "refresh_job",
                    "tasks": [
                        {
                            "task_key": "main",
                            "notebook_task": {
                                "notebook_path": "/Workspace/Users/user@example.test/projects/example/notebooks/refresh"
                            },
                        }
                    ],
                    "job_clusters": [
                        {
                            "job_cluster_key": "default",
                            "new_cluster": {
                                "spark_version": "13.3.x-scala2.12",
                                "node_type_id": "i3.xlarge",
                                "num_workers": 2,
                            },
                        }
                    ],
                },
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
    ),
    ("POST", "/api/2.1/statement-execution/execute"): (
        200,
        {"Content-Type": "application/json"},
        json.dumps(
            {
                "statement_id": "query-1234567890",
                "state": "AVAILABLE",
                "manifest": {
                    "format": "ROW_BASED",
                    "schema": [
                        {"name": "result", "type_text": "STRING", "type_json": '"string"'}
                    ],
                },
                "result": {"row_count": 1, "row_data": [["success"]]},
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
    ),
}


class MockHandler(http.server.BaseHTTPRequestHandler):
    """HTTP handler that returns recorded responses or 404/501 for unknown paths."""

    def do_GET(self):
        self._handle_request("GET")

    def do_POST(self):
        self._handle_request("POST")

    def do_PUT(self):
        self._handle_request("PUT")

    def do_PATCH(self):
        self._handle_request("PATCH")

    def do_DELETE(self):
        self._handle_request("DELETE")

    def _handle_request(self, method: str) -> None:
        """Route the request to a recorded response or return an error."""
        path = self.path
        key = (method, path)

        if key in RESPONSES:
            status_code, headers, body = RESPONSES[key]
            self.send_response(status_code)
            for header, value in headers.items():
                self.send_header(header, value)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body.encode())
        else:
            # Record that this was an unrecorded request
            status_code = 404 if method == "GET" else 501
            error_body = json.dumps(
                {
                    "error_code": "RESOURCE_NOT_FOUND"
                    if status_code == 404
                    else "NOT_IMPLEMENTED",
                    "message": f"No recorded response for {method} {path}",
                },
                separators=(",", ":"),
                sort_keys=True,
            )
            self.send_response(status_code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(error_body)))
            self.end_headers()
            self.wfile.write(error_body.encode())

    def log_message(self, format, *args):
        """Suppress default logging unless explicitly enabled."""
        if hasattr(sys.stderr, "isatty") and sys.stderr.isatty():
            super().log_message(format, *args)


class ReuseAddrServer(socketserver.TCPServer):
    """TCP server that allows address reuse."""

    allow_reuse_address = True


def find_free_port(host: str = "127.0.0.1") -> int:
    """Find an available port on the given host."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind((host, 0))
        s.listen(1)
        return s.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description="Mock Databricks workspace server")
    parser.add_argument(
        "--host", default="127.0.0.1", help="Host to bind to (default: 127.0.0.1)"
    )
    parser.add_argument(
        "--port",
        type=int,
        default=0,
        help="Port to bind to (default: 0 for ephemeral)",
    )
    args = parser.parse_args()

    host = args.host
    port = args.port if args.port != 0 else find_free_port(host)

    with ReuseAddrServer((host, port), MockHandler) as server:
        print(f"Mock workspace server listening on http://{host}:{port}")
        print(f"Recorded {len(RESPONSES)} responses")
        sys.stdout.flush()
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            print("Server stopped")


if __name__ == "__main__":
    main()
