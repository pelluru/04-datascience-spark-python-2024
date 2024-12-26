from confluent_kafka import Producer
from confluent_kafka.admin import AdminClient, NewTopic
from confluent_kafka.serialization import SerializationContext, MessageField
from confluent_kafka.schema_registry import SchemaRegistryClient, record_subject_name_strategy, \
    topic_record_subject_name_strategy
from confluent_kafka.schema_registry.avro import AvroSerializer
from confluent_kafka.schema_registry.json_schema import JSONSerializer
import sys
import json
import os


def create_topic(kafka_conf, topic_name):
    client = AdminClient(kafka_conf)
    topic_list = []
    topic_list.append(
        NewTopic(topic_name, num_partitions=1, replication_factor=1))
    client.create_topics(topic_list)


def delivery_report(err, msg):
    if err is not None:
        print("Delivery failed for User record {}: {}".format(msg.value(), err))
        return


def load_avro_json(encoded, schema):
    """Unlike `json.loads`, this decodes a json according to given avro schema.

    https://avro.apache.org/docs/1.11.1/specification/#json-encoding
    Specially, it handles `union` variants, and differentiates `bytes` from `string`.
    """
    from fastavro import json_reader
    from io import StringIO

    with StringIO(encoded) as buf:
        reader = json_reader(buf, schema)
        return next(reader)


if __name__ == '__main__':
    if len(sys.argv) <= 5:
        print(
            "usage: schema_registry_producer.py <brokerlist> <schema-registry-url> <file> <name-strategy> <json/avro>"
        )
        exit(1)
    broker_list = sys.argv[1]
    schema_registry_url = sys.argv[2]
    file = sys.argv[3]
    topic = os.path.basename(file).split(".")[0]
    type = sys.argv[5]
    if sys.argv[4] not in ['topic', 'record', 'topic-record', '', None]:
        print("name strategy must be one of: topic, record, topic_record")
        exit(1)
    if sys.argv[4] != 'topic':
        topic = topic + "-" + sys.argv[4]

    print("broker_list: {}".format(broker_list))
    print("schema_registry_url: {}".format(schema_registry_url))
    print("topic: {}".format(topic))
    print("name strategy: {}".format(
        sys.argv[4] if sys.argv[4] is not None else 'topic'))
    print("type: {}".format(type))

    schema_registry_conf = {'url': schema_registry_url}
    kafka_conf = {'bootstrap.servers': broker_list}
    schema_registry_client = SchemaRegistryClient(schema_registry_conf)

    avro_ser_conf = dict()
    if sys.argv[4] == 'record':
        avro_ser_conf['subject.name.strategy'] = record_subject_name_strategy
    elif sys.argv[4] == 'topic-record':
        avro_ser_conf['subject.name.strategy'] = topic_record_subject_name_strategy

    create_topic(kafka_conf=kafka_conf, topic_name=topic)
    producer = Producer(kafka_conf)
    key_serializer = None
    value_serializer = None
    key_schema = None
    value_schema = None
    with open(file) as file:
        for (i, line) in enumerate(file):
            if i == 0:
                parts = line.split("^")
                if len(parts) > 1:
                    if type == 'avro':
                        key_serializer = AvroSerializer(schema_registry_client=schema_registry_client,
                                                        schema_str=parts[0],
                                                        conf=avro_ser_conf)
                        value_serializer = AvroSerializer(schema_registry_client=schema_registry_client,
                                                          schema_str=parts[1],
                                                          conf=avro_ser_conf)
                        key_schema = json.loads(parts[0])
                        value_schema = json.loads(parts[1])
                    else:
                        key_serializer = JSONSerializer(schema_registry_client=schema_registry_client,
                                                        schema_str=parts[0])
                        value_serializer = JSONSerializer(schema_registry_client=schema_registry_client,
                                                          schema_str=parts[1])
                else:
                    if type == 'avro':
                        value_serializer = AvroSerializer(schema_registry_client=schema_registry_client,
                                                          schema_str=parts[0])
                        value_schema = json.loads(parts[0])
                    else:
                        value_serializer = JSONSerializer(schema_registry_client=schema_registry_client,
                                                          schema_str=parts[0])
            else:
                parts = line.split("^")
                key_idx = None
                value_idx = None
                if len(parts) > 1:
                    key_idx = 0
                    if len(parts[1].strip()) > 0:
                        value_idx = 1
                else:
                    value_idx = 0
                if type == 'avro':
                    if key_idx is not None:
                        key_json = load_avro_json(parts[key_idx], key_schema)
                    if value_idx is not None:
                        value_json = load_avro_json(parts[value_idx], value_schema)
                else:
                    if key_idx is not None:
                        key_json = json.loads(parts[key_idx])
                    if value_idx is not None:
                        value_json = json.loads(parts[value_idx])
                if len(parts) > 1:
                    if len(parts[1].strip()) > 0:
                        producer.produce(topic=topic, partition=0,
                                         key=key_serializer(key_json,
                                                            SerializationContext(topic, MessageField.KEY)),
                                         value=value_serializer(
                                             value_json, SerializationContext(topic, MessageField.VALUE)),
                                         on_delivery=delivery_report)
                    else:
                        producer.produce(topic=topic, partition=0,
                                         key=key_serializer(key_json,
                                                            SerializationContext(topic, MessageField.KEY)),
                                         on_delivery=delivery_report)
                else:
                    producer.produce(topic=topic, partition=0,
                                     value=value_serializer(
                                         value_json, SerializationContext(topic, MessageField.VALUE)),
                                     on_delivery=delivery_report)

    producer.flush()
