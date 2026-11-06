       IDENTIFICATION DIVISION.
       PROGRAM-ID. kwoo.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 OBJECT PIC X(6) VALUE "obj".
       01 NEW PIC 9.
       01 NULL PIC 9.
       01 PROPERTY PIC X.
       01 INVOKE PIC 9.
       PROCEDURE DIVISION.
       main-para.
           MOVE "y" TO OBJECT.
           MOVE 1 TO NEW.
           MOVE 0 TO NULL.
           MOVE "p" TO PROPERTY.
           MOVE 2 TO INVOKE.
           STOP RUN.
