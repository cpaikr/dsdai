#!/usr/bin/env python3
"""Build the synthetic public DSD fixture used by tests and examples.

Everything here is independently authored: a fictional company, invented
figures, and a structure that mirrors the constructs observed in real DART
Editor files (cover, TOC, sections, inserted libraries, THEAD/TBODY tables,
row/column spans, TE/TU cells, &cr; line breaks, page breaks, and an image
whose ZIP member name is CP949 bytes without the UTF-8 flag).

Usage: python make_fixture.py OUTPUT.dsd
"""

from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path
from typing import List, Tuple

IMAGE_NAME = "대표이사 확인서.jpg"
IMAGE_BYTES = b"\xff\xd8\xff\xe0synthetic-jpeg-placeholder\xff\xd9"

CONTENTS = """<?xml version="1.0" encoding="utf-8"?>


<DOCUMENT xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="dart4.xsd">

<DOCUMENT-HEADER AEXT-CLASS="Y">
<DOCUMENT-NAME ACODE="00760">감사보고서</DOCUMENT-NAME>
<FORMULA-VERSION ADATE="20221027">6.0</FORMULA-VERSION>
<COMPANY-NAME AACCOUNTTYPE="" AREGCIK=""></COMPANY-NAME>

<SUMMARY>
<EXTRACTION ACODE="FIN_STAT" AFEATURE="BOTH">100</EXTRACTION>
</SUMMARY>
</DOCUMENT-HEADER>

<BODY ATOCID="4">

<INSERTION ABASISNUMBER="SE1" ADUPLICATION="N" AFREQUENCY="0">
<COMMENT>◆click◆ 예시: 정정 안내 문단 삽입</COMMENT>
<LIBRARYLIST>예시-정정*.LSample</LIBRARYLIST>
</INSERTION>

<COVER>
<P USERMARK="F-18 ">가나다테크 주식회사</P>
<COVER-TITLE ATOC="Y" ATOCID="1">감   사   보   고   서</COVER-TITLE>

<TABLE-GROUP ACLASS="COVER" ADELETETABLE="N">

<TABLE ACLASS="EXTRACTION" AFIXTABLE="Y" BORDER="0" WIDTH="600">

<COLGROUP WIDTH="600">
<COL WIDTH="342"></COL>
<COL WIDTH="258"></COL>
</COLGROUP>

<TBODY>

<TR ACOPY="N" ADELETE="N" HEIGHT="30">
<TU ALIGN="RIGHT" AUNIT="PERIODFROM" AUNITVALUE="20250101" HEIGHT="23" WIDTH="333">2025년 01월 01일</TU>
<TD HEIGHT="23" WIDTH="249">부터</TD>
</TR>

<TR ACOPY="N" ADELETE="N" HEIGHT="30">
<TU ALIGN="RIGHT" AUNIT="PERIODTO" AUNITVALUE="20251231" HEIGHT="23" WIDTH="333">2025년 12월 31일</TU>
<TD HEIGHT="23" WIDTH="249">까지</TD>
</TR>
</TBODY>
</TABLE>
</TABLE-GROUP>
<P>&amp;cr;&amp;cr;</P>
<PGBRK></PGBRK>
</COVER>

<INSERTION ABASISNUMBER="E1" ADUPLICATION="N" AFREQUENCY="1">
<COMMENT>◆click◆ 『예시 감사인 보고서』 삽입</COMMENT>
<LIBRARYLIST>00000#*예시 감사인 보고서.dsl</LIBRARYLIST>

<LIBRARY>
<FILENAME ATYPE="dsl">01_예시 감사인 보고서</FILENAME>

<SECTION-1 ACLASS="MANDATORY" APARTSOURCE="SOURCE">
<TITLE ATOC="Y" ATOCID="2" AASSOCNOTE="D-0-0-9-0">독립된 감사인의 감사보고서</TITLE>
<P USERMARK="B">감사의견</P>
<P>우리는 가나다테크 주식회사(이하 &quot;회사&quot;)의 재무제표를 감사하였습니다.&amp;cr;둘째 줄입니다.</P>
<WARNING>※ 편집기 안내 문구입니다.</WARNING>

<TABLE ACLASS="NORMAL" AFIXTABLE="N" BORDER="1" WIDTH="600">

<COLGROUP WIDTH="600">
<COL WIDTH="300"></COL>
<COL WIDTH="300"></COL>
</COLGROUP>

<TBODY>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" WIDTH="291">
<P>가나다테크 주식회사</P>
</TD>
<TD HEIGHT="23" WIDTH="291">
<P>첫 문단</P>
<P>둘째 문단</P>
</TD>
</TR>
</TBODY>
</TABLE>
<PGBRK></PGBRK>
</SECTION-1>
</LIBRARY>
</INSERTION>

<SECTION-1 ACLASS="MANDATORY" APARTSOURCE="SOURCE">
<TITLE ATOC="Y" ATOCID="3">(첨부)재 무 제 표</TITLE>

<SECTION-2 ACLASS="MANDATORY">
<TITLE ATOC="Y" ATOCID="4">재무상태표</TITLE>

<TABLE ACLASS="NORMAL" AFIXTABLE="N" BORDER="1" WIDTH="626">

<COLGROUP WIDTH="626">
<COL WIDTH="256"></COL>
<COL WIDTH="185"></COL>
<COL WIDTH="185"></COL>
</COLGROUP>

<THEAD>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="23" WIDTH="247">과      목</TH>
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="23" WIDTH="176">제 5 기</TH>
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="23" WIDTH="176">제 4 기</TH>
</TR>
</THEAD>

<TBODY>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD ALIGN="CENTER" HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">자            산</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">　</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">　</TD>
</TR>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">Ⅰ.유동자산</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">1,500,000</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">1,200,000</TD>
</TR>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">   현금및현금성자산</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">1,000,000</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">700,000</TD>
</TR>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">   매출채권</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">500,000</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">500,000</TD>
</TR>

<TR ACOPY="N" ADELETE="N" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">자 산 총 계</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">1,500,000</TD>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="176">1,200,000</TD>
</TR>
</TBODY>
</TABLE>

<P>1. 일반사항</P>

<TABLE ACLASS="NORMAL" AFIXTABLE="N" BORDER="1" WIDTH="600">

<COLGROUP WIDTH="600">
<COL WIDTH="200"></COL>
<COL WIDTH="200"></COL>
<COL WIDTH="200"></COL>
</COLGROUP>

<THEAD>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="53" ROWSPAN="2" VALIGN="MIDDLE" WIDTH="191">주주명</TH>
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" COLSPAN="2" HEIGHT="23" VALIGN="MIDDLE" WIDTH="391">당기말</TH>
</TR>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">주식수(주)</TH>
<TH ACOPYCOL="Y" ADELETECOL="Y" AMOVECOL="N" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">지분율(%)</TH>
</TR>
</THEAD>

<TBODY>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">홍길동</TD>
<TE ACODE="SH5_NM" ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">60,000</TE>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">60.00%</TD>
</TR>

<TR ACOPY="Y" ADELETE="Y" HEIGHT="30">
<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">기타</TD>
<TE ACODE="SH5_NM" ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">40,000</TE>
<TD ALIGN="RIGHT" HEIGHT="23" VALIGN="MIDDLE" WIDTH="191">40.00%</TD>
</TR>
</TBODY>
</TABLE>

<IMAGE>
<IMG ALIGN="CENTER" HEIGHT="300" HSPACE="0" WIDTH="400">대표이사 확인서.jpg</IMG>
<IMG-CAPTION ATOC="N">대표이사 확인서</IMG-CAPTION>
</IMAGE>
<PGBRK></PGBRK>
</SECTION-2>
</SECTION-1>
</BODY>
</DOCUMENT>
"""

META = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<METAINFO>
\t<GENERATOR editver="5.114" schema="dart4.xsd"/>
\t<DOCUMENT-HEADER regcik="" regname=""/>
\t<DOCUMENT-INFO bsn-id="00760" doc-id="00760" docver="6.0" iscorrection="N" rpt-id="00760"/>
\t<FILE-INFO date="" rncheck="" rnimgfile="" rnnumber="" size=""/>
\t<DOCUMENT-IMG-LIST>
\t\t<IMG filename="대표이사 확인서.jpg"/>
\t</DOCUMENT-IMG-LIST>
</METAINFO>
"""


def crlf(text: str) -> bytes:
    return text.replace("\r\n", "\n").replace("\n", "\r\n").encode("utf-8")


def build_zip(members: List[Tuple[bytes, bytes]]) -> bytes:
    """Deflate ZIP with raw name bytes, no UTF-8 flag, fixed DOS timestamp."""
    dos_time, dos_date = (10 << 11) | (30 << 5), ((2026 - 1980) << 9) | (1 << 5) | 15
    parts, central, offset = [], [], 0
    for name, data in members:
        comp = zlib.compressobj(6, zlib.DEFLATED, -15)
        payload = comp.compress(data) + comp.flush()
        crc = zlib.crc32(data) & 0xFFFFFFFF
        local = struct.pack(
            "<4sHHHHHIIIHH", b"PK\x03\x04", 20, 0, 8, dos_time, dos_date, crc, len(payload), len(data), len(name), 0
        )
        central.append(
            struct.pack(
                "<4sHHHHHHIIIHHHHHII",
                b"PK\x01\x02", 20, 20, 0, 8, dos_time, dos_date, crc, len(payload), len(data),
                len(name), 0, 0, 0, 0, 0x20, offset,
            )
            + name
        )
        part = local + name + payload
        parts.append(part)
        offset += len(part)
    cd = b"".join(central)
    eocd = struct.pack("<4sHHHHIIH", b"PK\x05\x06", 0, 0, len(central), len(central), len(cd), offset, 0)
    return b"".join(parts) + cd + eocd


def fixture_bytes() -> bytes:
    return build_zip(
        [
            (b"contents.xml", crlf(CONTENTS)),
            (b"meta.xml", crlf(META)),
            (IMAGE_NAME.encode("cp949"), IMAGE_BYTES),
        ]
    )


def main(argv: List[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write("usage: make_fixture.py OUTPUT.dsd\n")
        return 2
    out = Path(argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(fixture_bytes())
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
