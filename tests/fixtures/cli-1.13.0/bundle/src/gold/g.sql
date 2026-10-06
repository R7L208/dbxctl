-- Gold layer metrics and KPIs
-- This table is refreshed daily for dashboard consumption

CREATE OR REPLACE TABLE gold.daily_metrics AS
SELECT
  CAST(CURRENT_DATE() AS DATE) as metric_date,
  COUNT(DISTINCT customer_id) as unique_customers,
  COUNT(DISTINCT order_id) as total_orders,
  ROUND(AVG(total_amount), 2) as avg_order_value,
  ROUND(SUM(total_amount), 2) as total_revenue
FROM gold.customer_orders;

-- Create view for dashboard
CREATE OR REPLACE VIEW gold.daily_metrics_view AS
SELECT
  metric_date,
  unique_customers,
  total_orders,
  avg_order_value,
  total_revenue,
  ROUND(total_revenue / NULLIF(unique_customers, 0), 2) as revenue_per_customer
FROM gold.daily_metrics
ORDER BY metric_date DESC;
