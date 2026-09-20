# brain/attachment: reconnect identity

Owns the durable record of who is attached to a Brain and what they've already seen: attachment
and connection identities, participant records, approval audience, and the reconnect cursor file
that lets a client resume its projection after dropping and reattaching, instead of replaying the
whole journal. It exists as its own facade because reconnect correctness — not losing or
duplicating what a client already saw — is easy to get wrong if it's mixed into general Brain
storage.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
