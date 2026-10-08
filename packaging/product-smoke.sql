CREATE EXTENSION vector;
CREATE EXTENSION pg_evidence;
SET statement_timeout = '10s';
SELECT evidence.init_collection('smoke_corpus', '{}'::jsonb);
DO $$
DECLARE
  asset uuid := '00000000-0000-0000-0000-000000000001';
  first_version uuid;
  citation uuid;
  second_version uuid;
BEGIN
  PERFORM evidence.stage_version('smoke_corpus', jsonb_build_object(
    'asset_id',asset,'path','source.txt','source','old café',
    'source_sha256',encode(sha256(convert_to('old café','UTF8')),'hex'),
    'spans',jsonb_build_array(jsonb_build_object('start_byte',0,'end_byte',9)),
    'ingestion_key','00000000-0000-0000-0000-000000000011','expected_revision',0));
  SELECT version_id INTO STRICT first_version FROM smoke_corpus.versions;
  SELECT evidence_id INTO STRICT citation FROM smoke_corpus.spans;
  PERFORM evidence.publish_version('smoke_corpus',jsonb_build_object('version_id',first_version));
  PERFORM evidence.stage_version('smoke_corpus', jsonb_build_object(
    'asset_id',asset,'path','source.txt','source','new text',
    'source_sha256',encode(sha256(convert_to('new text','UTF8')),'hex'),
    'spans',jsonb_build_array(jsonb_build_object('start_byte',0,'end_byte',8)),
    'ingestion_key','00000000-0000-0000-0000-000000000012','expected_revision',1));
  SELECT version_id INTO STRICT second_version FROM smoke_corpus.versions WHERE version_id <> first_version;
  PERFORM evidence.publish_version('smoke_corpus',jsonb_build_object('version_id',second_version));
  IF evidence.resolve('smoke_corpus',citation)::jsonb->>'status' <> 'historical' THEN
    RAISE EXCEPTION 'old citation did not retain historical status';
  END IF;
  IF (SELECT text FROM smoke_corpus.spans WHERE evidence_id=citation) <> 'old café' THEN
    RAISE EXCEPTION 'old citation bytes changed';
  END IF;
END $$;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE n.nspname='evidence' AND (p.prosecdef OR NOT p.proisstrict OR p.proparallel <> 'u')) THEN
    RAISE EXCEPTION 'unsafe function attributes';
  END IF;
END $$;
CREATE ROLE smoke_reader;
CREATE ROLE smoke_writer;
GRANT USAGE ON SCHEMA evidence, smoke_corpus TO smoke_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA smoke_corpus TO smoke_reader;
GRANT smoke_reader TO smoke_writer;
GRANT INSERT ON smoke_corpus.assets, smoke_corpus.versions, smoke_corpus.spans,
  smoke_corpus.publications, smoke_corpus.tags, smoke_corpus.relations TO smoke_writer;
GRANT UPDATE (current_version_id,current_path,content_revision,annotation_revision,retired_at)
  ON smoke_corpus.assets TO smoke_writer;
GRANT DELETE ON smoke_corpus.tags, smoke_corpus.relations TO smoke_writer;
SET ROLE smoke_reader;
SELECT evidence.resolve('smoke_corpus', evidence_id) FROM smoke_corpus.spans;
RESET ROLE;
SET ROLE smoke_writer;
DO $$ BEGIN
  BEGIN
    UPDATE smoke_corpus.spans SET text='tampered';
    RAISE EXCEPTION 'writer changed retained span bytes';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  BEGIN
    DELETE FROM smoke_corpus.versions;
    RAISE EXCEPTION 'writer deleted retained versions';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  -- The column-level grant must permit the API's row lock.
  PERFORM 1 FROM smoke_corpus.assets FOR UPDATE;
END $$;
RESET ROLE;
