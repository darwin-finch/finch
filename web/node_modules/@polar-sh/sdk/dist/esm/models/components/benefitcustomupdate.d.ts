import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitCustomProperties, BenefitCustomProperties$Outbound } from "./benefitcustomproperties.js";
export type BenefitCustomUpdate = {
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description?: string | null | undefined;
    type?: "custom" | undefined;
    properties?: BenefitCustomProperties | null | undefined;
};
/** @internal */
export declare const BenefitCustomUpdate$inboundSchema: z.ZodType<BenefitCustomUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitCustomUpdate$Outbound = {
    description?: string | null | undefined;
    type: "custom";
    properties?: BenefitCustomProperties$Outbound | null | undefined;
};
/** @internal */
export declare const BenefitCustomUpdate$outboundSchema: z.ZodType<BenefitCustomUpdate$Outbound, z.ZodTypeDef, BenefitCustomUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitCustomUpdate$ {
    /** @deprecated use `BenefitCustomUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitCustomUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitCustomUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitCustomUpdate$Outbound, z.ZodTypeDef, BenefitCustomUpdate>;
    /** @deprecated use `BenefitCustomUpdate$Outbound` instead. */
    type Outbound = BenefitCustomUpdate$Outbound;
}
export declare function benefitCustomUpdateToJSON(benefitCustomUpdate: BenefitCustomUpdate): string;
export declare function benefitCustomUpdateFromJSON(jsonString: string): SafeParseResult<BenefitCustomUpdate, SDKValidationError>;
//# sourceMappingURL=benefitcustomupdate.d.ts.map