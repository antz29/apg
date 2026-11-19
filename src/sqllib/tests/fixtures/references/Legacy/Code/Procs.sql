CREATE FUNCTION OrderTotal(@id INT) RETURNS DECIMAL(10, 2) AS
BEGIN
  RETURN (SELECT Total FROM Orders WHERE Id = @id)
END
GO
CREATE FUNCTION OrdersFor(@id INT) RETURNS TABLE AS
RETURN SELECT Id, Total FROM Orders WHERE Id = @id
GO
CREATE PROCEDURE Helper AS
SELECT 1
GO
CREATE PROCEDURE PlaceOrder @lines OrderLines READONLY, @phone Phone AS
DECLARE @id INT;
SELECT @id = NEXT VALUE FOR OrderIds;
INSERT INTO Orders (Id, ContactPhone, Total) SELECT @id, @phone, app.OrderTotal(@id);
EXEC Helper;
INSERT INTO OrderLog (Id, Note) SELECT Id, N'placed' FROM app.OrdersFor(@id);
GO
CREATE PROCEDURE CloseOrders AS
UPDATE o SET Total = 0 FROM Orders o JOIN Customers c ON c.Id = o.Id;
MERGE OrderLog AS t USING OrderView AS s ON t.Id = s.Id
WHEN NOT MATCHED THEN INSERT (Id, Note) VALUES (s.Id, N'merged');
GO
CREATE PROCEDURE Staging AS
CREATE TABLE #stage (Id INT, ContactPhone Phone);
DECLARE @tv TABLE (Id INT);
INSERT INTO #stage (Id) SELECT Id FROM Customers;
INSERT INTO @tv (Id) SELECT Id FROM #stage;
WITH recent AS (SELECT Id FROM Orders) SELECT Id FROM recent;
GO
CREATE PROCEDURE Dynamic AS
DECLARE @sql NVARCHAR(200) = N'SELECT Id FROM app.Orders';
EXEC sp_executesql @sql;
EXEC ('SELECT 1');
SELECT name FROM sys.tables;
SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES;
SELECT GETDATE();
GO
CREATE PROCEDURE ViaSynonyms AS
EXEC HelperSyn;
SELECT Id FROM OrdersSyn;
SELECT Id FROM FarSyn;
GO
CREATE TRIGGER OrdersAudit ON Orders AFTER UPDATE AS
INSERT INTO OrderLog (Id, Note) SELECT Id, N'updated' FROM inserted;
UPDATE Orders SET Total = 0 WHERE Id IN (SELECT Id FROM deleted);
