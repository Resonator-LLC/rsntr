-- camera-mod seed: projection point, policy, camera list config.
-- Apply AFTER `rsntr mod add cameras ...` (the config UPDATE needs the
-- cameras row):
--   rsntr sql <dir> --file examples/camera-mod/seed.sql
-- Idempotent. The runner splits on statement separators and knows
-- about string literals, quoted identifiers, and comments.
--
-- The mod needs no tables and no db capabilities. Each listed camera
-- must exist as a `_media` source on this node (rsntr media add) with a
-- browser-playable content type (fragmented mp4 and an avc1/mp4a codecs
-- string), and each viewer needs a per-source `_policy` media allow.
-- The list below is an example. Edit it to the node's real sources.

INSERT OR IGNORE INTO _projection
  (point_iri, kind, label, ord, modulation, signal, note)
VALUES
  ('urn:cameras:hologram', 'http://resonator.network/v3/rsntr#Hologram',
   'camera wall', 100, 'cameras', 'hologram', 'camera-mod seed');

-- Who may open the camera wall. '*' = every admitted peer sees the app;
-- the streams themselves stay gated per source by the media rows.
INSERT INTO _policy (peer_or_group, table_name, action, effect, note)
SELECT '*', '*', 'mod:cameras', 'allow', 'camera seed: open the wall'
WHERE NOT EXISTS (SELECT 1 FROM _policy
  WHERE peer_or_group = '*' AND table_name = '*' AND action = 'mod:cameras');

-- The camera list the app shows: JSON array of {name, label}, where
-- name is the _media source name. Edit to taste.
UPDATE _modulations
SET config = json_object('cameras', json('[
  {"name": "testcard", "label": "test card"}
]'))
WHERE name = 'cameras'
