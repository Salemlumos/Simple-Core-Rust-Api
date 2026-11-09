-- Invalidação de cache e despertar do worker via LISTEN/NOTIFY.
-- O NOTIFY disparado por trigger só é entregue no COMMIT, então nunca há
-- invalidação de algo que foi revertido.

CREATE FUNCTION notify_tenant_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_notify(
    'cache_invalidation',
    json_build_object('k', 'tenant', 'id', NEW.id, 'slug', NEW.slug)::text
  );
  IF NEW.status = 'provisioning'
     AND (TG_OP = 'INSERT' OR OLD.status IS DISTINCT FROM NEW.status) THEN
    PERFORM pg_notify('tenant_provisioning', NEW.id::text);
  END IF;
  RETURN NULL;
END;
$$;

CREATE TRIGGER tenants_notify
  AFTER INSERT OR UPDATE ON tenants
  FOR EACH ROW EXECUTE FUNCTION notify_tenant_change();

CREATE FUNCTION notify_platform_user_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_notify(
    'cache_invalidation',
    json_build_object('k', 'platform_user', 'id', COALESCE(NEW.id, OLD.id))::text
  );
  RETURN NULL;
END;
$$;

CREATE TRIGGER platform_users_notify
  AFTER UPDATE OR DELETE ON platform_users
  FOR EACH ROW EXECUTE FUNCTION notify_platform_user_change();
