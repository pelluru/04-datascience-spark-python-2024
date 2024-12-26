# Demo: Sinking to ClickHouse

In this demo, we want to showcase how RisingWave is able to sink data to ClickHouse.

1. Launch the cluster:

```sh
docker compose up -d
```

The cluster contains a RisingWave cluster and its necessary dependencies, a datagen that generates the data, a clichouse for sink.


2. Create the ClickHouse table:

```sh
./prepare.sh
```

3. Execute the SQL queries in sequence:

- create_source.sql
- create_mv.sql
- create_sink.sql

We only support `upsert` with clickhouse' `CollapsingMergeTree` and `VersionedCollapsingMergeTree`

4. Execute a simple query:

```sh
docker compose exec clickhouse-server clickhouse-client
```

```sql
select user_id, count(*) from default.demo_test group by user_id
```
