"""Minimal read-only Postgres MCP server for agent retrieval benchmarks."""

import os
from typing import Any

import psycopg
from mcp.server.fastmcp import FastMCP

DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://benchmark:benchmark@postgres:5432/benchmark"
)
HOST = os.environ.get("MCP_HTTP_HOST", "0.0.0.0")
PORT = int(os.environ.get("MCP_HTTP_PORT", "8899"))

mcp = FastMCP("postgres-benchmark-mcp", host=HOST, port=PORT)


def _connect():
    return psycopg.connect(DATABASE_URL)


@mcp.tool()
def list_tables(schema: str = "public") -> list[str]:
    """List table names in a schema."""
    with _connect() as conn, conn.cursor() as cur:
        cur.execute(
            """
            SELECT table_name FROM information_schema.tables
            WHERE table_schema = %s AND table_type = 'BASE TABLE'
            ORDER BY table_name
            """,
            (schema,),
        )
        return [row[0] for row in cur.fetchall()]


@mcp.tool()
def describe_table(table: str, schema: str = "public") -> list[dict[str, Any]]:
    """Describe columns for a table."""
    with _connect() as conn, conn.cursor() as cur:
        cur.execute(
            """
            SELECT column_name, data_type, is_nullable
            FROM information_schema.columns
            WHERE table_schema = %s AND table_name = %s
            ORDER BY ordinal_position
            """,
            (schema, table),
        )
        return [
            {"column": r[0], "type": r[1], "nullable": r[2] == "YES"}
            for r in cur.fetchall()
        ]


@mcp.tool()
def execute_sql(sql: str) -> dict[str, Any]:
    """Execute a read-only SQL query (SELECT / WITH only)."""
    normalized = sql.strip().lower()
    if not (normalized.startswith("select") or normalized.startswith("with")):
        raise ValueError("Only SELECT/WITH queries are allowed")
    with _connect() as conn, conn.cursor() as cur:
        cur.execute(sql)
        columns = [d[0] for d in cur.description] if cur.description else []
        rows = cur.fetchmany(500)
        return {"columns": columns, "rows": [list(r) for r in rows], "row_count": len(rows)}


if __name__ == "__main__":
    mcp.run(transport="streamable-http")
