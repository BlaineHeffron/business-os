# Invoice Ninja Example

This example starts a local Invoice Ninja instance for BusinessOS development.

The example does not include credentials.

The example does not use a configuration file.

## Start the services

1. Open a shell in this directory.
2. Generate the database values.

```sh
export INVOICE_NINJA_DB_PASSWORD="$(openssl rand -hex 24)"
export INVOICE_NINJA_DB_ROOT_PASSWORD="$(openssl rand -hex 24)"
```

3. Generate the application key.

```sh
export INVOICE_NINJA_APP_KEY="base64:$(openssl rand -base64 32)"
```

4. Set an invented local user address.

```sh
export INVOICE_NINJA_USER_EMAIL="user@example.test"
```

5. Read the local user password without showing the value.

```sh
read -r -s INVOICE_NINJA_USER_PASSWORD
export INVOICE_NINJA_USER_PASSWORD
```

6. Start the services.

```sh
docker compose up -d
```

7. Open `http://localhost:8003`.

8. Create an API token in the Invoice Ninja interface.

9. Store the API token outside this repository.

## Stop the services

Run `docker compose down`.

Add `--volumes` only when you intend to delete the local development data.
