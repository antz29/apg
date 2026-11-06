       IDENTIFICATION DIVISION.
       INTERFACE-ID. Printable INHERITS FROM Named.
       METHOD-ID. render.
       DATA DIVISION.
       LINKAGE SECTION.
       01 out-line PIC X(80).
       PROCEDURE DIVISION RETURNING out-line.
       END METHOD render.
       METHOD-ID. width.
       DATA DIVISION.
       LINKAGE SECTION.
       01 out-width PIC 9(3).
       PROCEDURE DIVISION RETURNING out-width.
       END METHOD width.
       END INTERFACE Printable.
