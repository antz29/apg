       IDENTIFICATION DIVISION.
       PROGRAM-ID. kwnames.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 METHOD PIC X(10) VALUE "m".
       01 FACTORY PIC 9(3) VALUE 1.
       01 INTERFACE PIC X.
       01 SELF PIC X(4).
       01 SUPER PIC X(4).
       01 GET PIC 9.
       01 INHERITS PIC 9.
       01 IMPLEMENTS PIC 9.
       PROCEDURE DIVISION.
       main-para.
           MOVE 2 TO FACTORY.
           MOVE "x" TO METHOD.
           IF SELF = SUPER
               MOVE 1 TO GET
           END-IF.
           DISPLAY METHOD FACTORY INTERFACE INHERITS IMPLEMENTS.
           STOP RUN.
