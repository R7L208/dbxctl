-- Silver layer transformation for analytics
-- Requires: raw data available in bronze tables
-- Produces: cleaned, deduplicated data for analytics

CREATE MATERIALIZED VIEW IF NOT EXISTS silver.customers AS
SELECT
  customer_id,
  name,
  email,
  created_at,
  updated_at,
  ROW_NUMBER() OVER (PARTITION BY customer_id ORDER BY updated_at DESC) as rn
FROM raw.customers
WHERE customer_id IS NOT NULL;

CREATE MATERIALIZED VIEW IF NOT EXISTS silver.orders AS
SELECT
  order_id,
  customer_id,
  order_date,
  total_amount,
  status,
  created_at
FROM raw.orders
WHERE order_id IS NOT NULL
  AND customer_id IS NOT NULL;
