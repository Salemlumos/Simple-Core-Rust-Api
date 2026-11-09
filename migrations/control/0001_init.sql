-- Banco control: tenants, usuários de plataforma, sessões e auditoria.

CREATE TABLE platform_users (
  id            uuid PRIMARY KEY,
  email         text NOT NULL UNIQUE CHECK (email = lower(email)),
  name          text NOT NULL,
  password_hash text NOT NULL,
  role          text NOT NULL CHECK (role IN ('owner', 'superuser')),
  is_active     boolean NOT NULL DEFAULT true,
  created_by    uuid REFERENCES platform_users(id) ON DELETE SET NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX platform_users_single_owner_idx
  ON platform_users (role) WHERE role = 'owner';

CREATE TABLE tenants (
  id         uuid PRIMARY KEY,
  slug       text NOT NULL UNIQUE
             CHECK (slug ~ '^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$'),
  name       text NOT NULL,
  db_name    text NOT NULL UNIQUE CHECK (db_name ~ '^tenant_[0-9a-f]{32}$'),
  status     text NOT NULL
             CHECK (status IN ('provisioning', 'active', 'suspended', 'failed')),
  last_error text,
  created_by uuid REFERENCES platform_users(id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE platform_refresh_tokens (
  id         uuid PRIMARY KEY,
  user_id    uuid NOT NULL REFERENCES platform_users(id) ON DELETE CASCADE,
  token_hash bytea NOT NULL UNIQUE,
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX platform_refresh_tokens_user_id_idx
  ON platform_refresh_tokens(user_id);

CREATE TABLE platform_audit (
  id          uuid PRIMARY KEY,
  actor_id    uuid NOT NULL,
  action      text NOT NULL,
  target_type text NOT NULL,
  target_id   text NOT NULL,
  payload     jsonb NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);
