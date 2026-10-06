# Databricks notebook source
# Refresh silver layer from raw data
# This notebook is called by the refresh_silver job

from delta import DeltaTable

# Refresh materialized views
spark.sql("""
  REFRESH MATERIALIZED VIEW silver.customers
""")

spark.sql("""
  REFRESH MATERIALIZED VIEW silver.orders
""")

print("Silver layer refresh complete")
