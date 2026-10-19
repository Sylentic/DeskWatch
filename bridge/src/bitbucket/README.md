Bitbucket Cloud REST API 2.0 responses for the tests: the shape of the
published swagger description (https://api.bitbucket.org/swagger.json),
trimmed to a few fields per object, with made-up workspaces, repositories,
ids and commits. They were written from that description, NOT captured from a
running Bitbucket, so they show what the bridge expects, not what a server was
seen to send. In particular the swagger does not list a `name` on a pipeline
step. Never paste real responses from a private or work repository here.
