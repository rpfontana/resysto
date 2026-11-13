# Configuration

Process settings tell a process how to run: which address to listen on, which database to connect to, which credentials to use. They come from environment variables, are loaded once when a command starts, and stop the command right away if something is missing or malformed. That way a bad deployment fails at startup with a clear message, not minutes later on the first request that needs the value.

Domain settings, such as how often a tenant polls a feed, are a different thing: they live in the database with permissions and audit, and aren't covered here.

## Naming

A setting is an environment variable named `RESYSTO_<SECTION>__<KEY>`. The double underscore separates levels, the single underscore stays inside names, and names map to lowercase fields:

```
RESYSTO_SMTP__HOST         -> smtp.host
RESYSTO_SMTP__FROM_ADDRESS -> smtp.from_address
RESYSTO_ADDR               -> addr
```

The double underscore is needed because variable names can only contain letters, digits and `_`: with a single `_`, `RESYSTO_SMTP_FROM_ADDRESS` could mean `smtp.from_address` or `smtp.from.address`.

## Loading

Each crate declares the settings it needs in a private struct, usually in its `settings.rs`, with one nested struct per section:

```rust
// crates/mailer/src/settings.rs
#[derive(Deserialize)]
pub(crate) struct Settings {
    pub(crate) smtp: Smtp,
}

#[derive(Deserialize)]
pub(crate) struct Smtp {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) password: SecretString,
}
```

The crate loads its settings as the first thing in the function that needs them, before any other work, so a problem shows up before anything has been touched:

```rust
pub async fn run() -> Result<(), Error> {
    let Settings { smtp } = resysto_config::load()?;
    // ...
}
```

`resysto_config::load` is the only code that reads the environment; don't call `std::env::var` anywhere else. The `resysto` binary only parses the command and calls the crate's function, it never reads settings itself. Tests don't read the environment either: they call an inner function that takes the values as arguments.

## Secrets

A secret is always a file. The variable holds the path to the file and ends in `_FILE`; the loader reads the file, trims trailing newlines, and stores the content in the field without the suffix:

```
RESYSTO_SMTP__PASSWORD_FILE=/run/secrets/smtp_password   -> smtp.password
```

Files, because environment values leak: they show up in `docker inspect` and `/proc/<pid>/environ`, are inherited by child processes and end up in crash reports. Docker Swarm already mounts secrets as files. We use `_FILE` in development too, so the name alone tells a reader that a value is a secret. Setting both `SMTP__PASSWORD` and `SMTP__PASSWORD_FILE` is an error.

A file that isn't secret, such as a public key, can use `_FILE` as well; its field is then a plain `String`.

Secret fields are `SecretString`: `Debug` prints `[REDACTED]`, and reading the value takes an explicit `expose_secret()`. Call it only where the value is consumed, don't store the result, and never log it:

```rust
let credentials = Credentials::new(smtp.user, smtp.password.expose_secret().to_owned());
```

## Development

Development values go in the `[env]` table of `resysto/.cargo/config.toml`, which is committed. Cargo applies it to `cargo run` and `cargo test` only, and a variable already set in your shell wins over it. `relative = true` resolves a path against `resysto/`, so commands work from any folder inside it:

```toml
[env]
RESYSTO_SMTP__HOST = "localhost"
RESYSTO_SMTP__PORT = "1025"
RESYSTO_SMTP__USER = "resysto"
RESYSTO_SMTP__PASSWORD_FILE = { value = ".secrets/smtp_password", relative = true }
```

Secret files go in `resysto/.secrets/`, which git ignores. Create them once:

```sh
printf dev-password > .secrets/smtp_password
```

The binary never loads a `.env` file itself: the development configuration lives in cargo, outside the binary, so the binary behaves the same everywhere. Run without cargo and without variables, it stops with `missing configuration field`.

## Production

The values live in Komodo: plain ones as stack variables, secrets as Docker Swarm secrets. Komodo renders the stack variables into a `.env` file next to the compose file, and the service picks them all up with `env_file`, so the compose file doesn't list them one by one:

```yaml
services:
  app:
    image: registry.example.com/resysto:${IMAGE_TAG}
    command: ["resysto", "serve"]
    env_file: [./.env]
    secrets: [smtp_password]

secrets:
  smtp_password: { external: true }
```

```
# .env, rendered by Komodo from the stack variables
RESYSTO_SMTP__HOST=smtp.example.com
RESYSTO_SMTP__PORT=587
RESYSTO_SMTP__USER=resysto
RESYSTO_SMTP__PASSWORD_FILE=/run/secrets/smtp_password
```

Docker reads this `.env` when it deploys the stack; the binary never sees the file, only the variables. A `_FILE` variable is just a path, so it sits with the plain values, while the secret itself is mounted from Swarm and never enters the `.env`. Swarm wants each secret listed on every service that mounts it; with several services, share the list with a YAML anchor.
