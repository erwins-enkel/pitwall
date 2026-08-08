# Changelog

## [0.4.0](https://github.com/erwins-enkel/pitwall/compare/pitwall-v0.3.2...pitwall-v0.4.0) (2026-08-06)


### Features

* support multiple runner prefixes with per-prefix repo mapping ([#56](https://github.com/erwins-enkel/pitwall/issues/56)) ([d01f35f](https://github.com/erwins-enkel/pitwall/commit/d01f35f735ff9e32ee4c6af02b061998b536e5ca))


### Bug Fixes

* sort docker runners by fleet then numeric index ([#59](https://github.com/erwins-enkel/pitwall/issues/59)) ([0a0b52f](https://github.com/erwins-enkel/pitwall/commit/0a0b52f3236c4d33f91763b2b296c82ee0214ea3)), closes [#58](https://github.com/erwins-enkel/pitwall/issues/58)

## [0.3.2](https://github.com/erwins-enkel/pitwall/compare/pitwall-v0.3.1...pitwall-v0.3.2) (2026-07-05)


### Bug Fixes

* **ui:** scale memory graph to its visible window max ([#47](https://github.com/erwins-enkel/pitwall/issues/47)) ([85700cd](https://github.com/erwins-enkel/pitwall/commit/85700cd3b593870067540f47add255fe89c69ade))

## [0.3.1](https://github.com/erwins-enkel/pitwall/compare/pitwall-v0.3.0...pitwall-v0.3.1) (2026-07-05)


### Bug Fixes

* **release:** reconcile release-please after publish ([#43](https://github.com/erwins-enkel/pitwall/issues/43)) ([141d380](https://github.com/erwins-enkel/pitwall/commit/141d380775c80a15cf15592dc8959e4590641ef2))
* **release:** split release-please across the tag boundary ([#45](https://github.com/erwins-enkel/pitwall/issues/45)) ([09cb94e](https://github.com/erwins-enkel/pitwall/commit/09cb94e2295a34774fbb91bb7dedc1792f586501))

## [0.3.0](https://github.com/erwins-enkel/pitwall/compare/pitwall-v0.2.0...pitwall-v0.3.0) (2026-07-05)


### Features

* **ui:** btop-style boxes around TUI sections ([#39](https://github.com/erwins-enkel/pitwall/issues/39)) ([daf9fed](https://github.com/erwins-enkel/pitwall/commit/daf9fed85617ce9186d7173a630f7f230c9512c7))
* **ui:** btop-style braille gradient sparklines ([#37](https://github.com/erwins-enkel/pitwall/issues/37)) ([ec3d0df](https://github.com/erwins-enkel/pitwall/commit/ec3d0df47d5b5bd536eba8465286c78418482181))
* **ui:** btop-style sparkline for the memory section ([#41](https://github.com/erwins-enkel/pitwall/issues/41)) ([0093b1f](https://github.com/erwins-enkel/pitwall/commit/0093b1f3a370d0e33d037b5086a348d5a2ba856e))
* vercel deployment build status in the TUI ([#38](https://github.com/erwins-enkel/pitwall/issues/38)) ([9f2fb74](https://github.com/erwins-enkel/pitwall/commit/9f2fb74812e55f9d306d740f6b724e7cbf2e05df))


### Bug Fixes

* draft-first release flow for immutable-releases compatibility ([#31](https://github.com/erwins-enkel/pitwall/issues/31)) ([211a315](https://github.com/erwins-enkel/pitwall/commit/211a3155e81c8465cc260903c4383506ff54c0d3))
* **ui:** rename runner-table header to "name" ([#40](https://github.com/erwins-enkel/pitwall/issues/40)) ([066e769](https://github.com/erwins-enkel/pitwall/commit/066e769d473ae4588c259608c983bcae23635c4a))

## [0.2.0](https://github.com/erwins-enkel/pitwall/compare/pitwall-v0.1.0...pitwall-v0.2.0) (2026-07-05)


### Features

* Catppuccin theming (4 flavors via PITWALL_THEME) ([#16](https://github.com/erwins-enkel/pitwall/issues/16)) ([12a7fa8](https://github.com/erwins-enkel/pitwall/commit/12a7fa8d35333e4ffdadafbcc52cc253d1a7e5cb))
* dummy-data screenshot + committed generator ([#22](https://github.com/erwins-enkel/pitwall/issues/22)) ([ee40c68](https://github.com/erwins-enkel/pitwall/commit/ee40c6864d5a2685d3c568c55e9e17297cb7805d))
* ellipsis-truncate flexing job & branch columns ([#5](https://github.com/erwins-enkel/pitwall/issues/5)) ([#20](https://github.com/erwins-enkel/pitwall/issues/20)) ([be31282](https://github.com/erwins-enkel/pitwall/commit/be31282effe752eb0a8ce04b876ca899151d3896))
* near-cap memory alerting ([#8](https://github.com/erwins-enkel/pitwall/issues/8)) ([#18](https://github.com/erwins-enkel/pitwall/issues/18)) ([c7c0200](https://github.com/erwins-enkel/pitwall/commit/c7c0200d814ffa251cda986f01489e01cdad5ead))
* optional TOML config file ([#6](https://github.com/erwins-enkel/pitwall/issues/6)) ([#14](https://github.com/erwins-enkel/pitwall/issues/14)) ([2941648](https://github.com/erwins-enkel/pitwall/commit/29416486473811f63a25b4b4d2d32e5d81f42e05))
* per-runner CPU/mem history sparklines ([#7](https://github.com/erwins-enkel/pitwall/issues/7)) ([#15](https://github.com/erwins-enkel/pitwall/issues/15)) ([285fb46](https://github.com/erwins-enkel/pitwall/commit/285fb462279edb756d6c7b91ea79f0f988406083))
* pitwall — pulse runner stats TUI ([#1](https://github.com/erwins-enkel/pitwall/issues/1)) ([e442927](https://github.com/erwins-enkel/pitwall/commit/e44292703d7d095ec204dcebaefe1f1b30cdb52d))
* poll multiple repos (comma-separated PITWALL_REPO / TOML array) ([#26](https://github.com/erwins-enkel/pitwall/issues/26)) ([959921d](https://github.com/erwins-enkel/pitwall/commit/959921dd5cde90817d082f51bff4cfd544544b07))
* self-diagnosing empty states for unset repo and prefix mismatch ([#17](https://github.com/erwins-enkel/pitwall/issues/17)) ([a5edf3a](https://github.com/erwins-enkel/pitwall/commit/a5edf3a3b01aa9485f88600f3546b9bdb6dd3e38))
* show branch each runner's job was started for ([#13](https://github.com/erwins-enkel/pitwall/issues/13)) ([0cdf12d](https://github.com/erwins-enkel/pitwall/commit/0cdf12d87cb0c2f118f07834647206a0e05fa68d))
* show GitHub-hosted job status (running + queued) ([#25](https://github.com/erwins-enkel/pitwall/issues/25)) ([9c44c34](https://github.com/erwins-enkel/pitwall/commit/9c44c349c1a52b96d5aa17f19072314fe25b9af8))
* show repo on hosted jobs when polling multiple repos ([#28](https://github.com/erwins-enkel/pitwall/issues/28)) ([bb085f1](https://github.com/erwins-enkel/pitwall/commit/bb085f1e4f02f6c2f8285f5c98ad938b70bacb77))
* support native (non-pulse) self-hosted runners ([#19](https://github.com/erwins-enkel/pitwall/issues/19)) ([bd6c604](https://github.com/erwins-enkel/pitwall/commit/bd6c604edbc28e5adf94b62ddd1735617bc17488))


### Bug Fixes

* auto-size runner column so the runner number stays visible ([#21](https://github.com/erwins-enkel/pitwall/issues/21)) ([784a263](https://github.com/erwins-enkel/pitwall/commit/784a2631d1f9f841c89723831eaf3306efe2eb57))
