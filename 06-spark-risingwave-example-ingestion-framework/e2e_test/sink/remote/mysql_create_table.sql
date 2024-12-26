CREATE TABLE t_remote_0 (
    id integer PRIMARY KEY,
    v_varchar varchar(100),
    v_smallint smallint,
    v_integer integer,
    v_bigint bigint,
    v_decimal decimal,
    v_float float,
    v_double double,
    v_timestamp DATETIME(6)
);

CREATE TABLE t_remote_1 (
    id BIGINT PRIMARY KEY,
    v_varchar VARCHAR(255),
    v_text TEXT,
    v_integer INT,
    v_smallint SMALLINT,
    v_bigint BIGINT,
    v_decimal DECIMAL(10,2),
    v_real FLOAT,
    v_double DOUBLE,
    v_boolean BOOLEAN,
    v_date DATE,
    v_time TIME(6),
    v_timestamp DATETIME(6),
    v_timestamptz TIMESTAMP(6),
    v_interval VARCHAR(255),
    v_jsonb JSON,
    v_bytea BLOB
);

CREATE TABLE t_types (
   id BIGINT PRIMARY KEY,
   varchar_column VARCHAR(100),
   text_column TEXT,
   integer_column INTEGER,
   smallint_column SMALLINT,
   bigint_column BIGINT,
   decimal_column DECIMAL(10,2),
   real_column float,
   double_column DOUBLE,
   boolean_column TINYINT,
   date_column DATE,
   time_column TIME,
   timestamp_column DATETIME,
   interval_column VARCHAR(100),
   jsonb_column JSON,
   array_column LONGTEXT,
   array_column2 LONGTEXT,
   array_column3 LONGTEXT,
   array_column4 LONGTEXT,
   array_column5 LONGTEXT,
   array_column6 LONGTEXT
);
