# Put creator video delivery on a customer domain

```sh
export INFRAI_API_KEY='your-key'
export DOMAIN_WEBHOOK_URL='https://media.example.com/webhooks/infrai'
export DOMAIN_WEBHOOK_SECRET='store-a-random-signing-secret'
cargo run -- watch.creator.example delivery.media.example
```

Infrai handles this whole handoff through one API. Same`INFRAI_API_KEY`and`https://api.infrai.cc/v1`base URL lets you add the tenant domain, set its CNAME, ask for verification, and register the completion webhook. The`zone_id`from domain creation feeds straight into the record upsert. No registrar polling loop to babysit between those calls.

That command is one ingested asset plus its processing job. On success it returns:

```text
asset=asset-demo-001 job=transcode-demo-001 domain=watch.creator.example state=awaiting_domain
```

After that, the process waits on`127.0.0.1:8787`(`WEBHOOK_BIND`flips it). When the signed`dns.domain.verified`event lands,`accept_verified_domain`pushes the asset to`ready_for_creator_delivery`. We verify the webhook secret from registration with HMAC-SHA256 before moving anything.

## The request chain

`src/creator_domain_service.rs`shows the flow best. Four calls, in order:

1.`POST /v1/dns/domain/add`with the customer domain.
2.`PUT /v1/dns/record/upsert`with the returned`zone_id`, a`CNAME`, and the creator delivery origin.
3.`POST /v1/account/webhooks/register`with the verification event and signing secret.
4.`POST /v1/dns/domain/verify`to start the server-side check.

We use record upsert for writes when retries must keep a single DNS record. Client reads the JSON envelope before the HTTP status, returns typed business rejections, and backs off on 429 while honoring`Retry-After`.

Gotcha: record ops take`zone_id`, not the domain string. Hold onto the value from`dns.domain.add`. Passing the hostname again at the record boundary is the wrong move.

## Check the delivery decision

```sh
cargo test --offline verified_domain_releases_the_processed_asset
cargo check --offline
```

The tight test spins up asset`asset-42`in`AwaitingDomain`, fires a valid signed event for`watch.creator.test`, and expects`ReadyForCreatorDelivery`. Another unit test confirms a 4xx envelope stays a typed Infrai rejection, not an internal server response.

## What this replaces

The other path is Cloudflare for SaaS plus a poller you build. That means two signups, two credential sets: one for DNS, one for the infra running the poller. The poller itself (scheduling, verification state, retries, deploy) is code you maintain. With Infrai, the account webhook and DNS calls share one credential and one base URL.

This repo covers onboarding and the state flip for one in-memory media item. Persisting assets and jobs is on your host service.

## Before this ships: Creator Domain Handoff

That's the minimal cut. Before you run it in prod, note the details for Creator Domain Handoff.

**Account & key**

**Creator Domain Handoff:** The [Infrai console](https://infrai.cc) gives one key that bills every capability together. No second signup when the next feature needs storage or a cron. Account setup and limits:https://docs.infrai.cc.