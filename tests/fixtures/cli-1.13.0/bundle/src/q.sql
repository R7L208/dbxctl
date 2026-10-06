-- Query table for reporting
-- This is a simple query layer that joins silver tables

CREATE TABLE IF NOT EXISTS gold.customer_orders AS
SELECT
  c.customer_id,
  c.name,
  c.email,
  COUNT(o.order_id) as total_orders,
  SUM(o.total_amount) as total_spent,
  MAX(o.order_date) as last_order_date
FROM silver.customers c
LEFT JOIN silver.orders o ON c.customer_id = o.customer_id
WHERE c.rn = 1  -- Get only the latest customer record
GROUP BY c.customer_id, c.name, c.email;
