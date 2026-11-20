-- Runs once on an empty PostgreSQL volume. Keep the application login separate
-- from the bootstrap superuser. psql quotes the password as a SQL literal.
\getenv app_password TIKKIT_DB_PASSWORD
CREATE ROLE tikkit LOGIN PASSWORD :'app_password';
CREATE DATABASE tikkit OWNER tikkit;
