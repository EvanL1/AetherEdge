# Product models moved

The canonical product-model JSON files are owned by the
[Energy Pack](../../../../packs/energy/models/). `aether-model` embeds no
domain products; runtime compositions load only the manifest-validated Packs
selected in the site configuration. This directory retains no JSON copies.

Remove this pointer after supported downstream links use the Pack path and the
empty `get_builtin_*` compatibility APIs are removed under ADR-0007's stated
criteria.
