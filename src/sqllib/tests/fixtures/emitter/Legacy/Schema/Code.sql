CREATE VIEW OrderTotals AS
SELECT o.id, o.total AS amount
FROM Orders o
GO
CREATE PROCEDURE GetOrders AS
SELECT id FROM Orders
SELECT total FROM Orders
GO
CREATE TRIGGER OrdersAudit ON sales.Audit AFTER INSERT AS
SELECT 1
