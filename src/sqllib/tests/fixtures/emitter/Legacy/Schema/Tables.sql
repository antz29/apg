:setvar Env Dev
CREATE TABLE Orders (
  id INT,
  total DECIMAL(10, 2)
)
GO
CREATE TABLE sales.Audit (id INT, note NVARCHAR(50))
GO
CREATE TYPE Lines AS TABLE (sku INT, qty INT)
