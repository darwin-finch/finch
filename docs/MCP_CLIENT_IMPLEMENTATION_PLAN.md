# Finch MCP client implementation plan

## Goal

Make Finch a reliable Model Context Protocol client. Finch starts configured
stdio MCP servers, discovers their tools, includes them in model requests, and
routes tool calls back to the owning server.

The supported transport is stdio. The existing legacy SSE configuration is
kept for compatibility but remains explicitly unsupported rather than pretending
to implement the current Streamable HTTP transport.

## Existing foundation

Finch already has an MCP client skeleton in `src/tools/mcp`: TOML configuration,
stdio child-process startup, initialization, tool discovery, executor routing, and
REPL management commands. The work therefore completes and hardens this path and
finishes the client integration.

## Implementation

1. Add shared MCP protocol constants.
2. Harden the stdio client:
   - negotiate a current protocol version and send the initialized notification;
   - serialize each request/response exchange, validate response IDs, ignore
     unrelated notifications while waiting, detect EOF, and apply timeouts;
   - preserve tool errors and non-text/structured tool results;
   - route names correctly when configured server names contain underscores;
   - initialize MCP in both REPL and one-shot query execution.
3. Make configuration operational everywhere:
   - initialize configured servers in both the REPL and one-shot query mode;
   - validate server names and settings with actionable diagnostics;
   - document configuration, environment variables, management commands, and
     troubleshooting.
4. Add focused protocol, routing, and configuration tests.

## Verification

- Run `cargo fmt --check`.
- Run MCP-focused unit and integration tests.
- Run `cargo check --tests` and the broader test suite where practical.
- Exercise the client against a deterministic local fixture server.

## Deliberate scope

- Resources, prompts, sampling, elicitation, tasks, and Streamable HTTP are not
  part of this change.
- Finch's existing OpenAI-compatible HTTP endpoint remains the supported way for
  external applications to drive Finch as a model. An MCP server should be added
  only when a concrete MCP-host integration requires Finch to be exposed as a tool.
