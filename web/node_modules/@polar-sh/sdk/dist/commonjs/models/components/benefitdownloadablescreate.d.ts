import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDownloadablesCreateProperties, BenefitDownloadablesCreateProperties$Outbound } from "./benefitdownloadablescreateproperties.js";
export type BenefitDownloadablesCreate = {
    type?: "downloadables" | undefined;
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description: string;
    /**
     * The ID of the organization owning the benefit. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
    properties: BenefitDownloadablesCreateProperties;
};
/** @internal */
export declare const BenefitDownloadablesCreate$inboundSchema: z.ZodType<BenefitDownloadablesCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitDownloadablesCreate$Outbound = {
    type: "downloadables";
    description: string;
    organization_id?: string | null | undefined;
    properties: BenefitDownloadablesCreateProperties$Outbound;
};
/** @internal */
export declare const BenefitDownloadablesCreate$outboundSchema: z.ZodType<BenefitDownloadablesCreate$Outbound, z.ZodTypeDef, BenefitDownloadablesCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitDownloadablesCreate$ {
    /** @deprecated use `BenefitDownloadablesCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitDownloadablesCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitDownloadablesCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitDownloadablesCreate$Outbound, z.ZodTypeDef, BenefitDownloadablesCreate>;
    /** @deprecated use `BenefitDownloadablesCreate$Outbound` instead. */
    type Outbound = BenefitDownloadablesCreate$Outbound;
}
export declare function benefitDownloadablesCreateToJSON(benefitDownloadablesCreate: BenefitDownloadablesCreate): string;
export declare function benefitDownloadablesCreateFromJSON(jsonString: string): SafeParseResult<BenefitDownloadablesCreate, SDKValidationError>;
//# sourceMappingURL=benefitdownloadablescreate.d.ts.map