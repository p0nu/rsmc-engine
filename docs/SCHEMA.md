# rsmc-engine — Database Schema

PostgreSQL schema, created by `migrations/0001_init.sql` and applied
automatically on startup when `auto_migrate` is enabled (it is, by default).
Migrations are embedded in the binary via `sqlx::migrate!`, so the running
service needs only database connectivity — no separate migration step.

Requires the `pgcrypto` extension (for `gen_random_uuid()`), created by the
migration.

## Enums

| Type                | Values                                                        |
|---------------------|---------------------------------------------------------------|
| `user_role`         | `admin`, `member`, `guest`                                    |
| `member_role`       | `owner`, `admin`, `member`                                    |
| `channel_type`      | `public`, `private`, `direct`, `group`                        |
| `notification_kind` | `mention`, `direct_message`, `channel_invite`, `thread_reply` |

## Tables

### `users`
Account records. Passwords are stored as Argon2id hashes — never plaintext.

- `id UUID PK` (default `gen_random_uuid()`)
- `email`, `username`, `display_name`
- `password_hash` (Argon2id)
- `role user_role`
- `avatar_url` (nullable)
- `is_active BOOL`
- `created_at`, `updated_at` (auto-touched by trigger)

Unique, case-insensitive indexes on `LOWER(email)` and `LOWER(username)`.

### `refresh_tokens`
One row per issued refresh token, enabling rotation and revocation. Only a hash
of the token (its fingerprint) is stored.

- `id UUID PK`
- `user_id → users(id)` `ON DELETE CASCADE`
- `token_hash` (unique fingerprint)
- `expires_at`, `revoked BOOL`, `created_at`

Index on `user_id`.

### `channels`
Channels and DMs. `name`/`topic` are null for direct channels.

- `id UUID PK`
- `name` (nullable), `topic` (nullable)
- `channel_type channel_type`
- `created_by → users(id)` `ON DELETE RESTRICT`
- `created_at`, `updated_at` (trigger-touched; also bumped on new messages)

Partial unique index on `LOWER(name)` where `name IS NOT NULL` (named channels
are unique; DMs aren't constrained).

### `channel_members`
Membership join table with per-channel role and read cursor.

- `channel_id → channels(id)` `ON DELETE CASCADE`
- `user_id → users(id)` `ON DELETE CASCADE`
- `role member_role`
- `last_read_at` (nullable; advanced when history is fetched)
- `joined_at`
- **PK** `(channel_id, user_id)`

Index on `user_id` (for "my channels").

### `messages`
Messages and thread replies. Soft-deleted (content cleared, `deleted_at` set).

- `id UUID PK`
- `channel_id → channels(id)` `ON DELETE CASCADE`
- `user_id → users(id)` `ON DELETE RESTRICT`
- `content TEXT`
- `parent_id → messages(id)` `ON DELETE CASCADE` (null = root; set = reply)
- `reply_count INT`
- `edited_at`, `deleted_at` (nullable), `created_at`

Indexes: `(channel_id, created_at DESC, id DESC)` for keyset history pagination;
partial index on `parent_id` for thread lookups.

### `attachments`
Uploaded files. `channel_id` is null until the file is bound to a message.
Bytes live on disk (`storage_path`); this table holds metadata.

- `id UUID PK`
- `uploader_id → users(id)` `ON DELETE RESTRICT`
- `channel_id → channels(id)` `ON DELETE SET NULL` (nullable)
- `filename`, `content_type`, `size_bytes`
- `storage_path` (server-local path; never serialized to clients)
- `created_at`

Index on `channel_id`.

### `message_attachments`
Join table linking messages to their attachments.

- `message_id → messages(id)` `ON DELETE CASCADE`
- `attachment_id → attachments(id)` `ON DELETE CASCADE`
- **PK** `(message_id, attachment_id)`

### `notifications`
Per-user notifications with a JSON payload.

- `id UUID PK`
- `user_id → users(id)` `ON DELETE CASCADE`
- `kind notification_kind`
- `payload JSONB`
- `read_at` (nullable), `created_at`

Indexes: `(user_id, created_at DESC)`; partial index on `user_id` where
`read_at IS NULL` (fast unread counts).

### `presence`
Last-seen / online state, one row per user. Live online status also comes from
the in-memory hub; this table backs "last seen" across restarts.

- `user_id UUID PK → users(id)` `ON DELETE CASCADE`
- `online BOOL`, `last_seen`

### `webhooks`
Outbound webhook subscriptions. `channel_id` null = all channels visible to the
owner. `secret` is used to HMAC-sign deliveries and is never serialized back.

- `id UUID PK`
- `owner_id → users(id)` `ON DELETE CASCADE`
- `channel_id → channels(id)` `ON DELETE CASCADE` (nullable)
- `target_url`
- `events TEXT[]` (e.g. `{message.created,message.deleted}`)
- `secret`, `is_active BOOL`, `created_at`

Indexes: on `channel_id`; partial index on `is_active` where `is_active`.

## Triggers

`set_updated_at()` keeps `updated_at` current on `users` and `channels`.

## Design notes

- **Soft deletes** on messages preserve thread structure and reply counts.
- **Keyset pagination** (not OFFSET) keeps history queries fast at any depth.
- **Cascade rules** are deliberate: deleting a user cascades their memberships,
  tokens, notifications, and presence, but message/channel authorship uses
  `RESTRICT` to avoid silently destroying shared history — deactivate users
  (`is_active = false`) instead of deleting them.
- **Extending the schema:** add new migration files as
  `migrations/000N_description.sql`. They run in lexical order at startup.
