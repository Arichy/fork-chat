This is a AI agent project. The biggest feature is it allows user to create a tree session. That't to say, every turn is a tree node and user could fork at any node. Every path in the tree ins a separate context to LLM api.

# Frontend

react + vite + shadcn + tanstack-query + tanstack-router.
Please use shadcn components.

# Backend

axum server + postgresql + sqlx.
In the init development, don't create more than one migration. If you need to update the schema, you should:

1. Tell me you'll update the schema, so I could disconnect other connections to db.
2. `sqlx database drop -y` to delete existing db.
3. `sqlx database create` to create a new db.
4. `sqlx migrate run` to run the new migration.
