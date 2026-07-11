# Change an AI-facing capability

1. Start with an application command/query behavior test.
2. Declare kind, risk, permission, idempotency, timeout, confirmation, and audit
   policy before implementing the handler.
3. Implement the application use case without transport-specific types.
4. Expose it through the shared capability registry.
5. Keep CLI, MCP, and optional HTTP handlers as thin translations.
6. Add an AI evaluation for allowed and denied behavior.
7. Regenerate schemas and verify that read-only clients cannot invoke commands.
