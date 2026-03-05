import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitCustomCreateProperties, BenefitCustomCreateProperties$Outbound } from "./benefitcustomcreateproperties.js";
/**
 * Schema to create a benefit of type `custom`.
 */
export type BenefitCustomCreate = {
    type?: "custom" | undefined;
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description: string;
    /**
     * The ID of the organization owning the benefit. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
    /**
     * Properties for creating a benefit of type `custom`.
     */
    properties: BenefitCustomCreateProperties;
};
/** @internal */
export declare const BenefitCustomCreate$inboundSchema: z.ZodType<BenefitCustomCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitCustomCreate$Outbound = {
    type: "custom";
    description: string;
    organization_id?: string | null | undefined;
    properties: BenefitCustomCreateProperties$Outbound;
};
/** @internal */
export declare const BenefitCustomCreate$outboundSchema: z.ZodType<BenefitCustomCreate$Outbound, z.ZodTypeDef, BenefitCustomCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitCustomCreate$ {
    /** @deprecated use `BenefitCustomCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitCustomCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitCustomCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitCustomCreate$Outbound, z.ZodTypeDef, BenefitCustomCreate>;
    /** @deprecated use `BenefitCustomCreate$Outbound` instead. */
    type Outbound = BenefitCustomCreate$Outbound;
}
export declare function benefitCustomCreateToJSON(benefitCustomCreate: BenefitCustomCreate): string;
export declare function benefitCustomCreateFromJSON(jsonString: string): SafeParseResult<BenefitCustomCreate, SDKValidationError>;
//# sourceMappingURL=benefitcustomcreate.d.ts.map