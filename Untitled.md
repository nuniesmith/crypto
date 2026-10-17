# Homelab manual to-do list

Oct 17, 2026 · @Jordan Smith

Start with the Sullivan SSH fix: until it is done, every audiobook you request downloads on Sullivan and never reaches Audiobookshelf.

## 1. Do first: let Sullivan accept the book-copy account

Sullivan's SSH hardening on 2026-09-22 left `shelfmark-sync` off its allow-list. That is the account freddy uses to copy finished books. Your *Other Worlds Than These* download is complete on Sullivan and stuck there, and the bot's "already on the server" reply was wrong.

- [ ] Run this from oryx or your Mac. It asks for your Sullivan password once:

```sh
ssh -t sullivan "sudo sed -i 's|^AllowUsers actions jordan claude\$|AllowUsers actions jordan claude shelfmark-sync@100.64.0.0/10|' /etc/ssh/sshd_config.d/00-sullivan-hardening.conf && sudo sshd -t && sudo systemctl reload ssh && grep ^AllowUsers /etc/ssh/sshd_config.d/00-sullivan-hardening.conf"
```

- [ ] Check the last line it prints ends in `shelfmark-sync@100.64.0.0/10`. If `sshd -t` reports an error, nothing reloads and your own access is unchanged.
- [ ] In #discordarr, type `/retry` and pick *Other Worlds Than These*. Its status message updates as it moves, and the book reaches Audiobookshelf a few minutes later. If `/retry` says nothing has failed, the automatic retry already picked it up.

## 2. Merge and deploy

Merge in this order. Each row says what happens on merge and whether you need a second step.

| PR | What it fixes | After you merge |
| --- | --- | --- |
| [scripts #19](https://github.com/nuniesmith/scripts/pull/19) | freddy's nightly backup has written nothing since Oct 9, because one line names LifeOS's old database container. | Nothing. Sullivan's sync picks it up within the hour and the 07:35 UTC run uses it. |
| [freddy #27](https://github.com/nuniesmith/freddy/pull/27) | Gives the Discord bot Emby's address and key, and the list of people allowed to delete. | freddy redeploys, so the house services restart for a minute. Merge it before discordarr #6. |
| [discordarr #6](https://github.com/nuniesmith/discordarr/pull/6) | Adds `/cleanup`: movies or shows nobody has watched in a year, biggest first, each with a confirmed Delete. | Deploys to freddy by itself. It works once the Emby key and your user id are set in section 3. |
| [crypto #27](https://github.com/nuniesmith/crypto/pull/27) | Restarts the trading loop if it dies, alerts Discord, and flags Kraken trades the bot didn't place. | Until the deploy token exists, run `gh workflow run ci-cd.yml -R nuniesmith/oryx` once the image build finishes. |

Already merged and live on 2026-10-17: shelfmark #42 and #43 (a status message per book, automatic retries, no false "already on the server"), discordarr #5 (`/retry`), sullivan #22 (the allow-list fix in the setup script), and sullivan #23 (Unpackerr's duplicate copies stopped; the janitor runs daily in report-only mode).

## 3. Keys and secrets

Only you can create these. Each `gh secret set` asks for the value, so run it in your own terminal.

- [ ] **Deploy token.** Create one fine-grained GitHub token with Actions read and write on `nuniesmith/freddy` and `nuniesmith/oryx`, then set it in two repos. Skip `nuniesmith/fks` while FKS stays down: a token there redeploys the whole FKS stack on its next merge.

```sh
gh secret set FREDDY_DEPLOY_TOKEN -R nuniesmith/lifeos
gh secret set ORYX_DEPLOY_TOKEN -R nuniesmith/crypto
```

- [ ] **Emby API key.** In Emby, open Settings, then Advanced, then API Keys, and add one named `sullivan`. Sullivan's copy lets Radarr, Sonarr and Lidarr refresh Emby after every import. freddy's copy lets the bot read watch history for the cleanup scan.

```sh
gh secret set EMBY_API_KEY -R nuniesmith/sullivan
gh secret set EMBY_API_KEY -R nuniesmith/freddy
```

- [ ] **Discord webhook for movies, shows and music.** In #discordarr, open Edit Channel, then Integrations, then Webhooks, and create one named `Media`. Radarr, Sonarr and Lidarr will post grabbed, imported and failed messages to it.

```sh
gh secret set DISCORD_MEDIA_WEBHOOK_URL -R nuniesmith/sullivan
```

- [ ] **qBittorrent login for the Sullivan janitor.** Use the same username and password shelfmark uses. Without them, the janitor still clears Unpackerr's copies, but it skips orphans and the old download area, because it can't prove no torrent owns them.

```sh
gh secret set QBITTORRENT_USERNAME -R nuniesmith/sullivan
gh secret set QBITTORRENT_PASSWORD -R nuniesmith/sullivan
```

- [ ] **Turn on the janitor's deletions, after reading its first report.** The report lands in #discordarr and lists everything it would delete. Secrets and this switch take effect on Sullivan's next deploy, so the second command applies them.

```sh
gh variable set MEDIA_JANITOR_EXECUTE -R nuniesmith/sullivan --body true
gh workflow run ci-cd.yml -R nuniesmith/sullivan
```

- [ ] **Who may delete with `/cleanup`.** In Discord, turn on Developer Mode under Settings, then Advanced. Right-click your own name and copy your user ID. Separate several IDs with commas. Until this is set, nobody can delete, though everyone can still see the list. It takes effect on freddy's next deploy.

```sh
gh secret set DISCORDARR_PURGE_USER_IDS -R nuniesmith/freddy
```

## 4. Decisions waiting on you

None of these is urgent. Each one blocks only the work in its row.

| Decision | Why it matters | Options |
| --- | --- | --- |
| Seed goal for IPTorrents | Prowlarr gives every arr a 3:1 goal with no time limit. A finished torrent keeps a second copy on the NVMe until it reaches the goal, and many never do. | After checking IPTorrents' hit-and-run rule, set a goal such as ratio 1.0 or 14 days: `gh variable set MEDIA_SEED_RATIO -R nuniesmith/sullivan --body 1.0`, `gh variable set MEDIA_SEED_DAYS -R nuniesmith/sullivan --body 14`, then redeploy Sullivan. Or keep 3:1. |
| Seed-category leftovers | 207 GB of old seed-category folders sit outside qBittorrent: 81 GB on the NVMe, 126 GB on the HDD pool. The janitor never touches the seed category. | Keep them, or delete them by hand once you know they're not needed. Your 160 force-seeded torrents live elsewhere and are unaffected. |
| FKS | Clearing Docker on 2026-10-17 deleted every FKS volume: fks\_db, janus\_db, and the Grafana and Prometheus history. | Retire it, or redeploy it from scratch. Leave the fks deploy secret unset until you choose. |
| LifeOS Notion import | A re-import overwrites edits made in LifeOS, because rows match on Notion page id. The production database has been empty since 2026-10-09. | Import once and switch to LifeOS for good, or keep Notion as the source for now. The script is `data/import-on-freddy.sh` from LifeOS PR #59. |
| Phone SSH key | Sullivan still accepts passwords over Tailscale only because your iPhone has no SSH key. | Add a key in the phone's SSH app, then delete the `Match Address` block from Sullivan's hardening file. |

## 5. After merging: quick checks

Everything you asked for is built. These checks confirm each piece works on the real system.

- [ ] Request an audiobook in Discord. One message should move through downloading, downloaded, copied to freddy, extracting and filing, and ready in Audiobookshelf.
- [ ] Type `/retry` with no job id. It lists failed jobs, or says nothing has failed.
- [ ] Read the janitor's report. It runs daily, and 10 minutes after every Sullivan deploy. It posts to #discordarr once the media webhook secret exists; until then, read it with \`ssh sullivan docker logs media-janitor\`. The first one should say about 600 GB of Unpackerr duplicates would be deleted, and flag the HDD pool at 9% free.
- [ ] After the Emby key and media webhook are set, request a movie. Discord should post when it is grabbed and when it is imported, and Emby should show it without a manual scan.
- [ ] Run `/cleanup type:Movies` once freddy #27 and discordarr #6 are merged and the keys are set. Read the list before deleting anything.
- [ ] Nothing to do for the crypto bot: after crypto #27 deploys, it posts to Discord only if its loop dies or a trade it didn't place appears.
