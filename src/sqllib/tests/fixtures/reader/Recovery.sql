-- Reader recovery fixture: SQLCMD, GO batches, one syntax error, a semicolon-free procedure.
:setvar DbName Sales
CREATE TABLE dbo.Orders
(
    Id INT NOT NULL,
    Db NVARCHAR(128) DEFAULT N'$(DbName)'
);
GO
CREATE TABLE dbo.Broken
(
    Id INT,,
    Name INT
);
GO
CREATE PROCEDURE dbo.GetOrders
AS
    SELECT Id FROM [$(DbName)].dbo.Orders
GO
