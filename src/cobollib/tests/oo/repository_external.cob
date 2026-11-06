       IDENTIFICATION DIVISION.
       PROGRAM-ID. jlist.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
           CLASS JavaList AS "java.util.ArrayList".
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 items OBJECT REFERENCE JavaList.
       PROCEDURE DIVISION.
       main-para.
           INVOKE JavaList "new" RETURNING items.
           STOP RUN.
