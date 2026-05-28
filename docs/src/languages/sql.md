---
title: SQL
description: "Configure SQL language support in Zed, including language servers, formatting, and debugging."
---

# SQL

SQL files are handled by the [SQL Extension](https://github.com/zed-extensions/sql).

- Tree-sitter: [nervenes/tree-sitter-sql](https://github.com/nervenes/tree-sitter-sql)

### Postgres Language Server

Zed can run [`postgres-language-server`](https://github.com/supabase-community/postgres-language-server) for `.sql` files to provide Postgres-compatible syntax diagnostics, completions, hover information, type checking, and formatting.

Install the SQL extension, then open a `.sql` file. Zed will use a `postgres-language-server` binary from your `PATH` if one is available, or download the matching binary from the official GitHub releases.

For schema-aware completions and type checking, provide a database connection using one of the environment variables supported by the language server, such as `DATABASE_URL`:

```sh
export DATABASE_URL="postgresql://postgres:postgres@localhost:5432/postgres"
```

You can also create a `postgres-language-server.jsonc` file at your project root:

```json
{
  "$schema": "https://pg-language-server.com/latest/schema.json",
  "db": {
    "host": "127.0.0.1",
    "port": 5432,
    "username": "postgres",
    "password": "postgres",
    "database": "postgres"
  }
}
```

### Formatting

Zed supports auto-formatting SQL using external tools like [`sql-formatter`](https://github.com/sql-formatter-org/sql-formatter).

1. Install `sql-formatter`:

```sh
npm install -g sql-formatter
```

2. Ensure `sql-formatter` is available in your path and check the version:

```sh
which sql-formatter
sql-formatter --version
```

3. Configure formatting in Settings ({#kb zed::OpenSettings}) under Languages > SQL, or add to your settings file:

```json [settings]
  "languages": {
    "SQL": {
      "formatter": {
        "external": {
          "command": "sql-formatter",
          "arguments": ["--language", "mysql"]
        }
      }
    }
  },
```

Substitute your preferred [SQL Dialect] for `mysql` above (`duckdb`, `hive`, `mariadb`, `postgresql`, `redshift`, `snowflake`, `sqlite`, `spark`, etc).

You can add this to Zed project settings (`.zed/settings.json`) or via your Zed user settings (`~/.config/zed/settings.json`).

### Advanced Formatting

Sql-formatter also allows more precise control by providing [sql-formatter configuration options](https://github.com/sql-formatter-org/sql-formatter#configuration-options). To provide these, create a `.sql-formatter.json` file in your project:

```json
{
  "language": "postgresql",
  "tabWidth": 2,
  "keywordCase": "upper",
  "linesBetweenQueries": 2
}
```

When using a `.sql-formatter.json` file you can use a simplified Zed settings configuration:

```json [settings]
{
  "languages": {
    "SQL": {
      "formatter": {
        "external": {
          "command": "sql-formatter"
        }
      }
    }
  }
}
```
