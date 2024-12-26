create secret postgres_pwd with (backend = 'meta') as '123456';

create table person (
    "id" int,
    "name" varchar,
    "email_address" varchar,
    "credit_card" varchar,
    "city" varchar,
    PRIMARY KEY ("id")
) with (
    connector = 'postgres-cdc',
    hostname = 'postgres',
    port = '5432',
    username = 'myuser',
    password = secret postgres_pwd,
    database.name = 'mydb',
    schema.name = 'public',
    table.name = 'person',
    slot.name = 'person'
);

CREATE SOURCE t_auction (
    id BIGINT,
    item_name VARCHAR,
    date_time BIGINT,
    seller INT,
    category INT
) WITH (
    connector = 'kafka',
    topic = 'auction',
    properties.bootstrap.server = 'message_queue:29092',
    scan.startup.mode = 'earliest'
) FORMAT PLAIN ENCODE JSON;

CREATE VIEW auction as
SELECT
    id,
    item_name,
    to_timestamp(date_time) as date_time,
    seller,
    category
FROM
    t_auction;

CREATE TABLE orders_rw (
    O_ORDERKEY BIGINT,
    O_CUSTKEY BIGINT,
    O_ORDERSTATUS VARCHAR,
    O_TOTALPRICE DECIMAL,
    O_ORDERDATE DATE,
    O_ORDERPRIORITY VARCHAR,
    O_CLERK VARCHAR,
    O_SHIPPRIORITY BIGINT,
    O_COMMENT VARCHAR,
    PRIMARY KEY (O_ORDERKEY)
) WITH (
    connector = 'postgres-cdc',
    hostname = 'postgres',
    port = '5432',
    username = 'myuser',
    password = secret postgres_pwd,
    database.name = 'mydb',
    schema.name = 'public',
    table.name = 'orders',
    slot.name = 'orders'
);
