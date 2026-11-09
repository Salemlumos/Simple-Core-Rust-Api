-- Banco de um tenant. Não há tenant_id: o isolamento é feito pelo próprio banco.

CREATE TABLE users (
  id              uuid PRIMARY KEY,
  email           text NOT NULL UNIQUE CHECK (email = lower(email)),
  name            text NOT NULL,
  password_hash   text NOT NULL,
  is_tenant_admin boolean NOT NULL DEFAULT false,
  is_active       boolean NOT NULL DEFAULT true,
  created_at      timestamptz NOT NULL DEFAULT now(),
  updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE modules (
  id   smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  code text NOT NULL UNIQUE,
  name text NOT NULL
);

CREATE TABLE features (
  id        smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  module_id smallint NOT NULL REFERENCES modules(id) ON DELETE RESTRICT,
  code      text NOT NULL UNIQUE,
  name      text NOT NULL
);

CREATE TABLE profiles (
  id          uuid PRIMARY KEY,
  name        text NOT NULL UNIQUE,
  description text,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE profile_features (
  profile_id uuid NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  feature_id smallint NOT NULL REFERENCES features(id) ON DELETE RESTRICT,
  PRIMARY KEY (profile_id, feature_id)
);

CREATE TABLE user_profiles (
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  profile_id uuid NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, profile_id)
);
CREATE INDEX user_profiles_profile_id_idx ON user_profiles(profile_id);

CREATE TABLE user_feature_overrides (
  user_id         uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  feature_id      smallint NOT NULL REFERENCES features(id) ON DELETE RESTRICT,
  effect          text NOT NULL CHECK (effect IN ('allow', 'deny')),
  reason          text NOT NULL,
  expires_at      timestamptz,
  created_by      uuid NOT NULL,
  created_by_type text NOT NULL CHECK (created_by_type IN ('platform', 'tenant')),
  created_at      timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, feature_id)
);

CREATE TABLE refresh_tokens (
  id         uuid PRIMARY KEY,
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash bytea NOT NULL UNIQUE,
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX refresh_tokens_user_id_idx ON refresh_tokens(user_id);

CREATE TABLE access_audit (
  id          uuid PRIMARY KEY,
  actor_id    uuid NOT NULL,
  actor_type  text NOT NULL CHECK (actor_type IN ('platform', 'tenant')),
  action      text NOT NULL,
  target_type text NOT NULL,
  target_id   text NOT NULL,
  payload     jsonb NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);
