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
from typing import Dict, Optional, Tuple
from urllib.parse import parse_qs, urlparse


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

    def _parse_url(self) -> Tuple[str, Dict[str, list]]:
        """Parse URL into path and query parameters."""
        parsed = urlparse(self.path)
        params = parse_qs(parsed.query)
        return parsed.path, params

    def _read_body(self) -> Optional[dict]:
        """Read and parse JSON body from request."""
        try:
            content_length = int(self.headers.get("Content-Length", 0))
            if content_length > 0:
                body = self.rfile.read(content_length)
                return json.loads(body.decode("utf-8"))
        except (ValueError, json.JSONDecodeError):
            pass
        return None

    def _handle_request(self, method: str) -> None:
        """Route the request to a recorded response or return an error."""
        path, params = self._parse_url()

        # Check for recorded exact match first
        if method == "GET" and path == "/api/2.0/pipelines/01a23b45c67d8901":
            return self._respond_pipeline()

        if method == "GET" and path == "/api/2.1/jobs/get":
            if params.get("job_id") == ["123"]:
                return self._respond_job()

        if method == "POST" and path == "/api/2.0/sql/statements":
            return self._respond_statement_execute()

        # Unrecorded path
        status_code = 404 if method == "GET" else 501
        error_body = json.dumps(
            {
                "error_code": "RESOURCE_NOT_FOUND"
                if status_code == 404
                else "NOT_IMPLEMENTED",
                "message": f"No recorded response for {method} {self.path}",
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(error_body)))
        self.end_headers()
        self.wfile.write(error_body.encode())

    def _respond_pipeline(self) -> None:
        """Return pipeline GET response."""
        body = json.dumps(
            {
                "channel": "CURRENT",
                "clusters": [
                    {
                        "aws_attributes": {
                            "availability": "SPOT",
                            "zone_id": "us-west-2b",
                        },
                        "label": "default",
                        "node_type_id": "i3.xlarge",
                        "num_workers": 0,
                        "spark_conf": {
                            "spark.databricks.cluster.profile": "singleNode"
                        },
                    }
                ],
                "configuration": {
                    "query_table": "table"
                },
                "created_at": 1698000000,
                "creator_user_id": 9876543210,
                "name": "dlt_pipeline",
                "object_id": "01a23b45c67d8901",
                "object_type": "PIPELINE",
                "pipeline_type": "TRIGGERED",
                "storage": "/Workspace/Users/user@example.test/projects/example-dev/storage",
                "updated_at": 1698100000,
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body.encode())

    def _respond_job(self) -> None:
        """Return job GET response."""
        body = json.dumps(
            {
                "created_time": 1698000000000,
                "creator_user_id": 9876543210,
                "job_id": 123,
                "run_as_principal_user_name": "user@example.test",
                "run_as_user_id": 9876543210,
                "settings": {
                    "job_clusters": [
                        {
                            "job_cluster_key": "default",
                            "new_cluster": {
                                "node_type_id": "i3.xlarge",
                                "num_workers": 2,
                                "spark_version": "13.3.x-scala2.12",
                            },
                        }
                    ],
                    "name": "refresh_job",
                    "tasks": [
                        {
                            "notebook_task": {
                                "notebook_path": "/Workspace/Users/user@example.test/projects/example-dev/files/notebooks/silver"
                            },
                            "task_key": "refresh_task",
                        }
                    ],
                },
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body.encode())

    def _respond_statement_execute(self) -> None:
        """Return SQL statement execution response."""
        body_data = self._read_body()
        body = json.dumps(
            {
                "manifest": {
                    "format": "ROW_BASED",
                    "schema": {
                        "column_count": 1,
                        "columns": [
                            {
                                "name": "result",
                                "position": 0,
                                "type_name": "STRING",
                                "type_text": "STRING",
                            }
                        ]
                    },
                    "total_chunk_count": 1
                },
                "result": {
                    "data_array": [["success"]]
                },
                "statement_id": "query-1234567890",
                "status": {
                    "state": "SUCCEEDED"
                },
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body.encode())

    def log_message(self, format, *args):
        """Suppress default logging unless explicitly enabled."""
        if hasattr(sys.stderr, "isatty") and sys.stderr.isatty():
            super().log_message(format, *args)


class SingleSocketServer(socketserver.TCPServer):
    """TCP server that allows address reuse and binds once."""

    allow_reuse_address = True
    daemon_threads = True


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
    port = args.port

    with SingleSocketServer((host, port), MockHandler) as server:
        actual_host, actual_port = server.server_address
        print(f"Mock workspace server listening on http://{actual_host}:{actual_port}")
        print(f"Recorded endpoints: GET /api/2.0/pipelines/01a23b45c67d8901, GET /api/2.1/jobs/get?job_id=123, POST /api/2.0/sql/statements")
        sys.stdout.flush()
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            print("Server stopped")


if __name__ == "__main__":
    main()
