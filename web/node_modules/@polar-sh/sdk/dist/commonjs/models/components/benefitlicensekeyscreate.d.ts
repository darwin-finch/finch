import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitLicenseKeysCreateProperties, BenefitLicenseKeysCreateProperties$Outbound } from "./benefitlicensekeyscreateproperties.js";
export type BenefitLicenseKeysCreate = {
    type?: "license_keys" | undefined;
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description: string;
    /**
     * The ID of the organization owning the benefit. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
    properties: BenefitLicenseKeysCreateProperties;
};
/** @internal */
export declare const BenefitLicenseKeysCreate$inboundSchema: z.ZodType<BenefitLicenseKeysCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitLicenseKeysCreate$Outbound = {
    type: "license_keys";
    description: string;
    organization_id?: string | null | undefined;
    properties: BenefitLicenseKeysCreateProperties$Outbound;
};
/** @internal */
export declare const BenefitLicenseKeysCreate$outboundSchema: z.ZodType<BenefitLicenseKeysCreate$Outbound, z.ZodTypeDef, BenefitLicenseKeysCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitLicenseKeysCreate$ {
    /** @deprecated use `BenefitLicenseKeysCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitLicenseKeysCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitLicenseKeysCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitLicenseKeysCreate$Outbound, z.ZodTypeDef, BenefitLicenseKeysCreate>;
    /** @deprecated use `BenefitLicenseKeysCreate$Outbound` instead. */
    type Outbound = BenefitLicenseKeysCreate$Outbound;
}
export declare function benefitLicenseKeysCreateToJSON(benefitLicenseKeysCreate: BenefitLicenseKeysCreate): string;
export declare function benefitLicenseKeysCreateFromJSON(jsonString: string): SafeParseResult<BenefitLicenseKeysCreate, SDKValidationError>;
//# sourceMappingURL=benefitlicensekeyscreate.d.ts.map