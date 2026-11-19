CREATE PROCEDURE dbo.ListOrders AS
SELECT Id, Total FROM dbo.Orders;
SELECT Id FROM app.Orders;
GO
CREATE PROCEDURE dbo.CountOrders AS
SELECT Id FROM Orders;
EXEC dbo.ListOrders;
