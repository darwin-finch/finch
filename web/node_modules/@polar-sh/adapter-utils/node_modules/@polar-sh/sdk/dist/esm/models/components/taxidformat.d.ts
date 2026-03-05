import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
/**
 * List of supported tax ID formats.
 *
 * @remarks
 *
 * Ref: https://docs.stripe.com/billing/customer/tax-ids#supported-tax-id
 */
export declare const TaxIDFormat: {
    readonly AdNrt: "ad_nrt";
    readonly AeTrn: "ae_trn";
    readonly ArCuit: "ar_cuit";
    readonly AuAbn: "au_abn";
    readonly AuArn: "au_arn";
    readonly BgUic: "bg_uic";
    readonly BhVat: "bh_vat";
    readonly BoTin: "bo_tin";
    readonly BrCnpj: "br_cnpj";
    readonly BrCpf: "br_cpf";
    readonly CaBn: "ca_bn";
    readonly CaGstHst: "ca_gst_hst";
    readonly CaPstBc: "ca_pst_bc";
    readonly CaPstMb: "ca_pst_mb";
    readonly CaPstSk: "ca_pst_sk";
    readonly CaQst: "ca_qst";
    readonly ChUid: "ch_uid";
    readonly ChVat: "ch_vat";
    readonly ClTin: "cl_tin";
    readonly CnTin: "cn_tin";
    readonly CoNit: "co_nit";
    readonly CrTin: "cr_tin";
    readonly DeStn: "de_stn";
    readonly DoRcn: "do_rcn";
    readonly EcRuc: "ec_ruc";
    readonly EgTin: "eg_tin";
    readonly EsCif: "es_cif";
    readonly EuOssVat: "eu_oss_vat";
    readonly EuVat: "eu_vat";
    readonly GbVat: "gb_vat";
    readonly GeVat: "ge_vat";
    readonly HkBr: "hk_br";
    readonly HrOib: "hr_oib";
    readonly HuTin: "hu_tin";
    readonly IdNpwp: "id_npwp";
    readonly IlVat: "il_vat";
    readonly InGst: "in_gst";
    readonly IsVat: "is_vat";
    readonly JpCn: "jp_cn";
    readonly JpRn: "jp_rn";
    readonly JpTrn: "jp_trn";
    readonly KePin: "ke_pin";
    readonly KrBrn: "kr_brn";
    readonly KzBin: "kz_bin";
    readonly LiUid: "li_uid";
    readonly MxRfc: "mx_rfc";
    readonly MyFrp: "my_frp";
    readonly MyItn: "my_itn";
    readonly MySst: "my_sst";
    readonly NgTin: "ng_tin";
    readonly NoVat: "no_vat";
    readonly NoVoec: "no_voec";
    readonly NzGst: "nz_gst";
    readonly OmVat: "om_vat";
    readonly PeRuc: "pe_ruc";
    readonly PhTin: "ph_tin";
    readonly RoTin: "ro_tin";
    readonly RsPib: "rs_pib";
    readonly RuInn: "ru_inn";
    readonly RuKpp: "ru_kpp";
    readonly SaVat: "sa_vat";
    readonly SgGst: "sg_gst";
    readonly SgUen: "sg_uen";
    readonly SiTin: "si_tin";
    readonly SvNit: "sv_nit";
    readonly ThVat: "th_vat";
    readonly TrTin: "tr_tin";
    readonly TwVat: "tw_vat";
    readonly UaVat: "ua_vat";
    readonly UsEin: "us_ein";
    readonly UyRuc: "uy_ruc";
    readonly VeRif: "ve_rif";
    readonly VnTin: "vn_tin";
    readonly ZaVat: "za_vat";
};
/**
 * List of supported tax ID formats.
 *
 * @remarks
 *
 * Ref: https://docs.stripe.com/billing/customer/tax-ids#supported-tax-id
 */
export type TaxIDFormat = ClosedEnum<typeof TaxIDFormat>;
/** @internal */
export declare const TaxIDFormat$inboundSchema: z.ZodNativeEnum<typeof TaxIDFormat>;
/** @internal */
export declare const TaxIDFormat$outboundSchema: z.ZodNativeEnum<typeof TaxIDFormat>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace TaxIDFormat$ {
    /** @deprecated use `TaxIDFormat$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly AdNrt: "ad_nrt";
        readonly AeTrn: "ae_trn";
        readonly ArCuit: "ar_cuit";
        readonly AuAbn: "au_abn";
        readonly AuArn: "au_arn";
        readonly BgUic: "bg_uic";
        readonly BhVat: "bh_vat";
        readonly BoTin: "bo_tin";
        readonly BrCnpj: "br_cnpj";
        readonly BrCpf: "br_cpf";
        readonly CaBn: "ca_bn";
        readonly CaGstHst: "ca_gst_hst";
        readonly CaPstBc: "ca_pst_bc";
        readonly CaPstMb: "ca_pst_mb";
        readonly CaPstSk: "ca_pst_sk";
        readonly CaQst: "ca_qst";
        readonly ChUid: "ch_uid";
        readonly ChVat: "ch_vat";
        readonly ClTin: "cl_tin";
        readonly CnTin: "cn_tin";
        readonly CoNit: "co_nit";
        readonly CrTin: "cr_tin";
        readonly DeStn: "de_stn";
        readonly DoRcn: "do_rcn";
        readonly EcRuc: "ec_ruc";
        readonly EgTin: "eg_tin";
        readonly EsCif: "es_cif";
        readonly EuOssVat: "eu_oss_vat";
        readonly EuVat: "eu_vat";
        readonly GbVat: "gb_vat";
        readonly GeVat: "ge_vat";
        readonly HkBr: "hk_br";
        readonly HrOib: "hr_oib";
        readonly HuTin: "hu_tin";
        readonly IdNpwp: "id_npwp";
        readonly IlVat: "il_vat";
        readonly InGst: "in_gst";
        readonly IsVat: "is_vat";
        readonly JpCn: "jp_cn";
        readonly JpRn: "jp_rn";
        readonly JpTrn: "jp_trn";
        readonly KePin: "ke_pin";
        readonly KrBrn: "kr_brn";
        readonly KzBin: "kz_bin";
        readonly LiUid: "li_uid";
        readonly MxRfc: "mx_rfc";
        readonly MyFrp: "my_frp";
        readonly MyItn: "my_itn";
        readonly MySst: "my_sst";
        readonly NgTin: "ng_tin";
        readonly NoVat: "no_vat";
        readonly NoVoec: "no_voec";
        readonly NzGst: "nz_gst";
        readonly OmVat: "om_vat";
        readonly PeRuc: "pe_ruc";
        readonly PhTin: "ph_tin";
        readonly RoTin: "ro_tin";
        readonly RsPib: "rs_pib";
        readonly RuInn: "ru_inn";
        readonly RuKpp: "ru_kpp";
        readonly SaVat: "sa_vat";
        readonly SgGst: "sg_gst";
        readonly SgUen: "sg_uen";
        readonly SiTin: "si_tin";
        readonly SvNit: "sv_nit";
        readonly ThVat: "th_vat";
        readonly TrTin: "tr_tin";
        readonly TwVat: "tw_vat";
        readonly UaVat: "ua_vat";
        readonly UsEin: "us_ein";
        readonly UyRuc: "uy_ruc";
        readonly VeRif: "ve_rif";
        readonly VnTin: "vn_tin";
        readonly ZaVat: "za_vat";
    }>;
    /** @deprecated use `TaxIDFormat$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly AdNrt: "ad_nrt";
        readonly AeTrn: "ae_trn";
        readonly ArCuit: "ar_cuit";
        readonly AuAbn: "au_abn";
        readonly AuArn: "au_arn";
        readonly BgUic: "bg_uic";
        readonly BhVat: "bh_vat";
        readonly BoTin: "bo_tin";
        readonly BrCnpj: "br_cnpj";
        readonly BrCpf: "br_cpf";
        readonly CaBn: "ca_bn";
        readonly CaGstHst: "ca_gst_hst";
        readonly CaPstBc: "ca_pst_bc";
        readonly CaPstMb: "ca_pst_mb";
        readonly CaPstSk: "ca_pst_sk";
        readonly CaQst: "ca_qst";
        readonly ChUid: "ch_uid";
        readonly ChVat: "ch_vat";
        readonly ClTin: "cl_tin";
        readonly CnTin: "cn_tin";
        readonly CoNit: "co_nit";
        readonly CrTin: "cr_tin";
        readonly DeStn: "de_stn";
        readonly DoRcn: "do_rcn";
        readonly EcRuc: "ec_ruc";
        readonly EgTin: "eg_tin";
        readonly EsCif: "es_cif";
        readonly EuOssVat: "eu_oss_vat";
        readonly EuVat: "eu_vat";
        readonly GbVat: "gb_vat";
        readonly GeVat: "ge_vat";
        readonly HkBr: "hk_br";
        readonly HrOib: "hr_oib";
        readonly HuTin: "hu_tin";
        readonly IdNpwp: "id_npwp";
        readonly IlVat: "il_vat";
        readonly InGst: "in_gst";
        readonly IsVat: "is_vat";
        readonly JpCn: "jp_cn";
        readonly JpRn: "jp_rn";
        readonly JpTrn: "jp_trn";
        readonly KePin: "ke_pin";
        readonly KrBrn: "kr_brn";
        readonly KzBin: "kz_bin";
        readonly LiUid: "li_uid";
        readonly MxRfc: "mx_rfc";
        readonly MyFrp: "my_frp";
        readonly MyItn: "my_itn";
        readonly MySst: "my_sst";
        readonly NgTin: "ng_tin";
        readonly NoVat: "no_vat";
        readonly NoVoec: "no_voec";
        readonly NzGst: "nz_gst";
        readonly OmVat: "om_vat";
        readonly PeRuc: "pe_ruc";
        readonly PhTin: "ph_tin";
        readonly RoTin: "ro_tin";
        readonly RsPib: "rs_pib";
        readonly RuInn: "ru_inn";
        readonly RuKpp: "ru_kpp";
        readonly SaVat: "sa_vat";
        readonly SgGst: "sg_gst";
        readonly SgUen: "sg_uen";
        readonly SiTin: "si_tin";
        readonly SvNit: "sv_nit";
        readonly ThVat: "th_vat";
        readonly TrTin: "tr_tin";
        readonly TwVat: "tw_vat";
        readonly UaVat: "ua_vat";
        readonly UsEin: "us_ein";
        readonly UyRuc: "uy_ruc";
        readonly VeRif: "ve_rif";
        readonly VnTin: "vn_tin";
        readonly ZaVat: "za_vat";
    }>;
}
//# sourceMappingURL=taxidformat.d.ts.map