-- Result-projection comparison: extension still validates and renders its envelope.
SELECT evidence.query('bench_smoke', '{"nodes":[{"id":"hits","op":"literal","text":"retrieval","limit":10}],"output":"hits","excerpt_bytes":1024}'::jsonb)::jsonb->'results';
