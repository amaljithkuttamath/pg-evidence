-- pg-evidence v0.1: nested roles, once per cluster/database (docs/operations.md).
-- Run as a role allowed to create roles; names are psql variables:
--   psql -v reader=evidence_reader -v writer=evidence_writer \
--        -v purger=evidence_purger -f sql/roles.sql
-- Reader within writer within purger. NOLOGIN group roles: grant them to
-- login roles. Function EXECUTE is PUBLIC by default; schema USAGE is not.
\set ON_ERROR_STOP on
BEGIN;
CREATE ROLE :"reader" NOLOGIN;
CREATE ROLE :"writer" NOLOGIN;
CREATE ROLE :"purger" NOLOGIN;
GRANT :"reader" TO :"writer";
GRANT :"writer" TO :"purger";
GRANT USAGE ON SCHEMA evidence TO :"reader";
COMMIT;
