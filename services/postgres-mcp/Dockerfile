FROM python:3.12-slim

RUN pip install --no-cache-dir "mcp[cli]<2" psycopg[binary]

WORKDIR /app
COPY server.py .

ENV MCP_TRANSPORT=http
ENV MCP_HTTP_HOST=0.0.0.0
ENV MCP_HTTP_PORT=8899

EXPOSE 8899
CMD ["python", "server.py"]
