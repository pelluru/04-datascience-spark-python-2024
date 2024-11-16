from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("pyspark word count example").master("local[1]").getOrCreate()

spark.read.csv("customer.csv").show(10,False)

