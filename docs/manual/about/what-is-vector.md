---
title: "What is Vector?"
description: "High-level description of the Vector observability data collector and router."
---

<SVG src="/img/components.svg" />

Vector is a lightweight, ultra-fast, [open-source][urls.vector_repo] tool for
building observability pipelines. Compared to Logstash and friends, Vector
[improves throughput by ~10X while significantly reducing CPU and memory
usage][urls.vector_performance].

### Principles

- **Reliability First.** - Built in [Rust][urls.rust], Vector's primary design goal is reliability.
- **One Tool. All Data.** - One simple tool gets your [logs][docs.data-model.log], [metrics][docs.data-model.metric], and traces (coming soon) from A to B.
- **Single Responsibility.** - Vector is a _data router_, it does not plan to become a distributed processing framework.

### Who should use Vector?

- You _SHOULD_ use Vector to replace Logstash, Fluent\*, Telegraf, Beats, or similar tools.
- You _SHOULD_ use Vector as a [daemon][docs.strategies#daemon] or [sidecar][docs.strategies#sidecar].
- You _SHOULD_ use Vector as a Kafka consumer/producer for observability data.
- You _SHOULD_ use Vector in resource constrained environments (such as devices).
- You _SHOULD NOT_ use Vector if you need an advanced distributed stream processing framework.
- You _SHOULD NOT_ use Vector to replace Kafka. Vector is designed to work with Kafka!
- You _SHOULD NOT_ use Vector for non-observability data such as analytics data.

### Community

- Vector is **downloaded over 100,000 times per day**.
- Vector's largest user **processes over 10TB daily**.
- Vector is **used by multiple fortune 500 companies** with stringent production requirements.
- Vector has **over 15 active contributors** and growing.

<Jump to="/guides/getting-started/">Get started</Jump>

[docs.data-model.log]: /docs/about/data-model/log/
[docs.data-model.metric]: /docs/about/data-model/metric/
[docs.strategies#daemon]: /docs/setup/deployment/strategies/#daemon
[docs.strategies#sidecar]: /docs/setup/deployment/strategies/#sidecar
[urls.rust]: https://www.rust-lang.org/
[urls.vector_performance]: https://vector.dev/#performance
[urls.vector_repo]: https://github.com/timberio/vector
