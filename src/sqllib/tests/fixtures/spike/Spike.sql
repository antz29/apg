-- Self-written SSDT-style spike fixture: SQLCMD lines, GO batches, semicolon-free procedure.
:setvar DbName SpikeDb
GO
CREATE TABLE dbo.Orders
(
    Id INT NOT NULL PRIMARY KEY,
    Total DECIMAL(10, 2) NULL
)
GO
CREATE PROCEDURE dbo.GetOrders
AS
BEGIN
    SELECT Id, Total FROM dbo.Orders
END
GO
:r .\Other.sql
GO
