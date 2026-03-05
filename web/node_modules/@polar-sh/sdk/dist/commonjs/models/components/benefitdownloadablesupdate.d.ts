import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDownloadablesCreateProperties, BenefitDownloadablesCreateProperties$Outbound } from "./benefitdownloadablescreateproperties.js";
export type BenefitDownloadablesUpdate = {
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description?: string | null | undefined;
    type?: "downloadables" | undefined;
    properties?: BenefitDownloadablesCreateProperties | null | undefined;
};
/** @internal */
export declare const BenefitDownloadablesUpdate$inboundSchema: z.ZodType<BenefitDownloadablesUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitDownloadablesUpdate$Outbound = {
    description?: string | null | undefined;
    type: "downloadables";
    properties?: BenefitDownloadablesCreateProperties$Outbound | null | undefined;
};
/** @internal */
export declare const BenefitDownloadablesUpdate$outboundSchema: z.ZodType<BenefitDownloadablesUpdate$Outbound, z.ZodTypeDef, BenefitDownloadablesUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitDownloadablesUpdate$ {
    /** @deprecated use `BenefitDownloadablesUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitDownloadablesUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitDownloadablesUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitDownloadablesUpdate$Outbound, z.ZodTypeDef, BenefitDownloadablesUpdate>;
    /** @deprecated use `BenefitDownloadablesUpdate$Outbound` instead. */
    type Outbound = BenefitDownloadablesUpdate$Outbound;
}
export declare function benefitDownloadablesUpdateToJSON(benefitDownloadablesUpdate: BenefitDownloadablesUpdate): string;
export declare function benefitDownloadablesUpdateFromJSON(jsonString: string): SafeParseResult<BenefitDownloadablesUpdate, SDKValidationError>;
//# sourceMappingURL=benefitdownloadablesupdate.d.ts.map